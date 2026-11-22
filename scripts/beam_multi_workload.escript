#!/usr/bin/env escript
%%! +S 2:2 +A 1 +SDcpu 1:1 +SDio 1 +P 32768 +Q 1024
-mode(compile).

main(Arguments) ->
    try
        need(Arguments =:= []),
        need(erlang:system_info(otp_release) =:= "29"),
        io:setopts(standard_io, [binary]),
        Input = case file:read(standard_io, 65537) of
            {ok, Data} -> Data;
            eof -> throw(invalid);
            _ -> error(input_read)
        end,
        need(byte_size(Input) =< 65536),
        Config = decode(Input),
        validate(Config),
        Result = run(Config),
        emit(Result),
        case maps:get(status, Result) of <<"ok">> -> halt(0); _ -> halt(1) end
    catch
        throw:invalid -> emit(#{version => 2, status => <<"invalid">>}), halt(2);
        _:_ -> emit(#{version => 2, status => <<"oracle_error">>}), halt(3)
    end.

need(true) -> ok;
need(false) -> throw(invalid).

decode(Input) ->
    try
        {Value, ok, Rest} = json:decode(Input, ok, #{
            object_start => fun(_) -> #{} end,
            object_push => fun(Key, Item, Object) -> need(not maps:is_key(Key, Object)), Object#{Key => Item} end,
            object_finish => fun(Object, Acc) -> {Object, Acc} end
        }),
        need(string:trim(Rest) =:= <<>>), Value
    catch _:_ -> throw(invalid) end.

keys(Value, Expected) ->
    need(is_map(Value)), need(lists:sort(maps:keys(Value)) =:= lists:sort(Expected)).

i64(Value) -> need(is_integer(Value) andalso Value >= -9223372036854775808 andalso Value =< 9223372036854775807).

validate(Config) ->
    keys(Config, [<<"version">>, <<"actors">>, <<"state_bytes">>, <<"initial">>, <<"requests">>]),
    need(maps:get(<<"version">>, Config) =:= 2),
    Count = maps:get(<<"actors">>, Config),
    need(is_integer(Count) andalso Count >= 1 andalso Count =< 10000),
    Bytes = maps:get(<<"state_bytes">>, Config),
    need(lists:member(Bytes, [256, 4096, 65536]) andalso Count * Bytes =< 16777216),
    i64(maps:get(<<"initial">>, Config)),
    Requests = maps:get(<<"requests">>, Config),
    need(is_list(Requests) andalso length(Requests) =< 256),
    lists:foreach(fun(Request) ->
        keys(Request, [<<"actor">>, <<"delta">>, <<"fail">>]),
        Identity = maps:get(<<"actor">>, Request),
        need(is_integer(Identity) andalso Identity >= 1 andalso Identity =< Count),
        i64(maps:get(<<"delta">>, Request)), need(is_boolean(maps:get(<<"fail">>, Request)))
    end, Requests).

run(Config) ->
    Count = maps:get(<<"actors">>, Config),
    Bytes = maps:get(<<"state_bytes">>, Config),
    Initial = maps:get(<<"initial">>, Config),
    Parent = self(),
    Deadline = erlang:monotonic_time(millisecond) + 3000,
    Entries = [begin
        {Pid, Monitor} = spawn_opt(fun() ->
            State = <<Initial:64/signed-big, Identity:64, 0:((Bytes - 16) * 8)>>,
            counter(Parent, Identity, State)
        end, [monitor, {max_heap_size, #{size => 262144, kill => true, error_logger => false, include_shared_binaries => true}}]),
        {Identity, Pid, Monitor}
    end || Identity <- lists:seq(1, Count)],
    Routes = maps:from_list([{Identity, Pid} || {Identity, Pid, _} <- Entries]),
    Monitors = maps:from_list([{Pid, {Identity, Monitor}} || {Identity, Pid, Monitor} <- Entries]),
    {Replies, Failures, Values} = batches(maps:get(<<"requests">>, Config), 1, Routes, Monitors, #{}, #{}, [], Deadline),
    lists:foreach(fun({Identity, Pid, _}) ->
        case maps:is_key(Identity, Failures) of false -> Pid ! {stop, Parent}; true -> ok end
    end, Entries),
    Committed = stops(Count - map_size(Failures), Monitors, Values, Deadline),
    #{version => 2, status => case map_size(Failures) of 0 -> <<"ok">>; _ -> <<"failure">> end,
      actors => Count, state_bytes => Bytes, message_bytes => 32, states_verified => true,
      committed => [maps:get(Identity, Committed) || Identity <- lists:seq(1, Count)],
      failures => [maps:get(Identity, Failures) || Identity <- lists:sort(maps:keys(Failures))],
      replies => lists:sort(fun(Left, Right) -> maps:get(seq, Left) < maps:get(seq, Right) end, Replies),
      environment => #{otp => <<"29">>, schedulers => erlang:system_info(schedulers_online), window => 32}}.

counter(Parent, Identity, <<Committed:64/signed-big, Identity:64, Padding/binary>> = State) ->
    receive
        {request, Parent, <<Sequence:64, Operation:64, Delta:64/signed-big, 0:64>>} ->
            Candidate = Committed + Delta,
            case Candidate < -9223372036854775808 orelse Candidate > 9223372036854775807 of
                true -> exit({handler_failed, overflow, Sequence, Committed}); false -> ok
            end,
            CandidateState = <<Candidate:64/signed-big, Identity:64, Padding/binary>>,
            case Operation of
                2 -> exit({handler_failed, injected_failure, Sequence, Committed});
                1 -> Parent ! {reply, self(), <<Sequence:64, 3:64, Candidate:64/signed-big, 0:64>>}, counter(Parent, Identity, CandidateState)
            end;
        {stop, Parent} ->
            true = byte_size(State) >= 256,
            true = Padding =:= binary:copy(<<0>>, byte_size(Padding)),
            exit({completed, Committed});
        _ -> exit(protocol_error)
    end.

batches([], _Sequence, _Routes, _Monitors, Failures, Values, Replies, _Deadline) -> {Replies, Failures, Values};
batches(Requests, Sequence, Routes, Monitors, Failures, Values, Replies, Deadline) ->
    {Batch, Rest} = lists:split(min(32, length(Requests)), Requests),
    {_Next, Expected} = lists:foldl(fun(Request, {Current, Pending}) ->
        Identity = maps:get(<<"actor">>, Request),
        case maps:is_key(Identity, Failures) of
            true -> {Current + 1, Pending};
            false ->
                Pid = maps:get(Identity, Routes),
                Operation = case maps:get(<<"fail">>, Request) of false -> 1; true -> 2 end,
                Delta = maps:get(<<"delta">>, Request),
                Pid ! {request, self(), <<Current:64, Operation:64, Delta:64/signed-big, 0:64>>},
                {Current + 1, maps:update_with(Identity, fun(Old) -> Old ++ [Current] end, [Current], Pending)}
        end
    end, {Sequence, #{}}, Batch),
    {NextFailures, NextValues, NextReplies} = collect(Expected, Monitors, Failures, Values, Replies, Deadline),
    batches(Rest, Sequence + length(Batch), Routes, Monitors, NextFailures, NextValues, NextReplies, Deadline).

collect(Expected, _Monitors, Failures, Values, Replies, _Deadline) when map_size(Expected) =:= 0 -> {Failures, Values, Replies};
collect(Expected, Monitors, Failures, Values, Replies, Deadline) ->
    receive
        {reply, Pid, <<Sequence:64, 3:64, Value:64/signed-big, 0:64>>} ->
            {Identity, _} = maps:get(Pid, Monitors),
            [Sequence | Tail] = maps:get(Identity, Expected),
            Next = case Tail of [] -> maps:remove(Identity, Expected); _ -> Expected#{Identity => Tail} end,
            collect(Next, Monitors, Failures, Values#{Identity => Value}, [#{actor => Identity, seq => Sequence, value => Value} | Replies], Deadline);
        {'DOWN', Monitor, process, Pid, {handler_failed, Reason, Failed, Committed}} ->
            {Identity, Monitor} = maps:get(Pid, Monitors),
            [Failed | _] = maps:get(Identity, Expected),
            Failure = #{actor => Identity, reason => Reason, failed_seq => Failed},
            collect(maps:remove(Identity, Expected), Monitors, Failures#{Identity => Failure}, Values#{Identity => Committed}, Replies, Deadline);
        _ -> error(protocol_error)
    after remaining(Deadline) -> error(deadline) end.

stops(0, _Monitors, Values, _Deadline) -> Values;
stops(Count, Monitors, Values, Deadline) ->
    receive
        {'DOWN', Monitor, process, Pid, {completed, Committed}} ->
            {Identity, Monitor} = maps:get(Pid, Monitors),
            stops(Count - 1, Monitors, Values#{Identity => Committed}, Deadline);
        _ -> error(protocol_error)
    after remaining(Deadline) -> error(deadline) end.

remaining(Deadline) -> max(0, Deadline - erlang:monotonic_time(millisecond)).
emit(Value) -> io:put_chars([json:encode(Value), $\n]).
