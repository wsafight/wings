#!/usr/bin/env escript
%%! +S 1:1 +A 1 +SDcpu 1:1 +SDio 1 +P 1024 +Q 1024
-mode(compile).

-define(MIN_I64, -9223372036854775808).
-define(MAX_I64, 9223372036854775807).
-define(MAX_INPUT, 65536).

main(Arguments) ->
    try
        require(Arguments =:= [], arguments),
        require(erlang:system_info(otp_release) =:= "29", otp_29_required),
        io:setopts(standard_io, [binary]),
        Input = case file:read(standard_io, ?MAX_INPUT + 1) of
            {ok, Bytes} when byte_size(Bytes) =< ?MAX_INPUT -> Bytes;
            _ -> throw({invalid, input_bytes})
        end,
        Config = decode(Input),
        validate(Config),
        Result = run(Config),
        emit(Result),
        case maps:get(status, Result) of
            <<"ok">> -> halt(0);
            <<"failure">> -> halt(1)
        end
    catch
        throw:{invalid, Reason} ->
            emit(#{version => 1, status => <<"invalid">>, reason => Reason}),
            halt(2);
        _Class:_Reason ->
            emit(#{version => 1, status => <<"oracle_error">>}),
            halt(3)
    end.

decode(Input) ->
    try
        {Value, ok, Rest} = json:decode(Input, ok, #{
            object_start => fun(_) -> #{} end,
            object_push => fun(Key, Item, Object) ->
                require(not maps:is_key(Key, Object), duplicate_key),
                Object#{Key => Item}
            end,
            object_finish => fun(Object, Acc) -> {Object, Acc} end
        }),
        require(string:trim(Rest) =:= <<>>, trailing_json),
        Value
    catch
        _:_ -> throw({invalid, json})
    end.

require(true, _) -> ok;
require(false, Reason) -> throw({invalid, Reason}).

keys(Value, Expected) ->
    require(is_map(Value), object),
    require(lists:sort(maps:keys(Value)) =:= lists:sort(Expected), fields).

i64(Value) ->
    require(is_integer(Value) andalso Value >= ?MIN_I64 andalso Value =< ?MAX_I64, i64).

validate(Config) ->
    keys(Config, [<<"version">>, <<"state_bytes">>, <<"initial">>, <<"requests">>]),
    require(maps:get(<<"version">>, Config) =:= 1, version),
    require(lists:member(maps:get(<<"state_bytes">>, Config), [256, 4096, 65536]), state_bytes),
    i64(maps:get(<<"initial">>, Config)),
    Requests = maps:get(<<"requests">>, Config),
    require(is_list(Requests), requests),
    require(length(Requests) =< 256, request_count),
    lists:foreach(fun(Request) ->
        keys(Request, [<<"delta">>, <<"fail">>]),
        i64(maps:get(<<"delta">>, Request)),
        require(is_boolean(maps:get(<<"fail">>, Request)), fail)
    end, Requests).

run(Config) ->
    Initial = maps:get(<<"initial">>, Config),
    StateBytes = maps:get(<<"state_bytes">>, Config),
    Parent = self(),
    {Actor, Monitor} = spawn_opt(fun() ->
        State = <<Initial:64/signed-big, 1:64, 0:((StateBytes - 16) * 8)>>,
        counter(Parent, State, 1)
    end, [monitor, {max_heap_size, #{size => 262144, kill => true,
                                   error_logger => false, include_shared_binaries => true}}]),
    Deadline = erlang:monotonic_time(millisecond) + 3000,
    Result = batches(maps:get(<<"requests">>, Config), Actor, Monitor, 1, [], Deadline),
    Result#{version => 1, state_bytes => StateBytes, message_bytes => 32,
            environment => #{otp => list_to_binary(erlang:system_info(otp_release)),
                             erts => list_to_binary(erlang:system_info(version)),
                             architecture => list_to_binary(erlang:system_info(system_architecture)),
                             emulator => erlang:system_info(emu_flavor),
                             schedulers => erlang:system_info(schedulers_online),
                             word_bytes => erlang:system_info(wordsize)}}.

counter(Parent, <<Committed:64/signed-big, Tail/binary>>, Expected) ->
    receive
        {request, Parent, <<Expected:64, Operation:64, Delta:64/signed-big, 0:64>>}
                when Operation =:= 1; Operation =:= 2 ->
            Candidate = Committed + Delta,
            case Candidate < ?MIN_I64 orelse Candidate > ?MAX_I64 of
                true -> exit({handler_failed, overflow, Expected, Committed});
                false -> ok
            end,
            CandidateState = <<Candidate:64/signed-big, Tail/binary>>,
            case Operation of
                2 -> exit({handler_failed, injected_failure, Expected, Committed});
                1 ->
                    Parent ! {reply, self(), <<Expected:64, 3:64, Candidate:64/signed-big, 0:64>>},
                    counter(Parent, CandidateState, Expected + 1)
            end;
        {stop, Parent} -> exit({completed, Committed});
        _ -> exit(protocol_error)
    end.

batches([], Actor, Monitor, _Sequence, Replies, Deadline) ->
    Actor ! {stop, self()},
    receive
        {'DOWN', Monitor, process, Actor, {completed, Committed}} ->
            #{status => <<"ok">>, committed => Committed, replies => lists:reverse(Replies)}
    after remaining(Deadline) -> error(deadline)
    end;
batches(Requests, Actor, Monitor, Sequence, Replies, Deadline) ->
    {Batch, Rest} = lists:split(min(32, length(Requests)), Requests),
    lists:foldl(fun(Request, Current) ->
        Operation = case maps:get(<<"fail">>, Request) of false -> 1; true -> 2 end,
        Delta = maps:get(<<"delta">>, Request),
        Actor ! {request, self(), <<Current:64, Operation:64, Delta:64/signed-big, 0:64>>},
        Current + 1
    end, Sequence, Batch),
    collect(length(Batch), Rest, Actor, Monitor, Sequence, Replies, Deadline).

collect(0, Rest, Actor, Monitor, Sequence, Replies, Deadline) ->
    batches(Rest, Actor, Monitor, Sequence, Replies, Deadline);
collect(Count, Rest, Actor, Monitor, Sequence, Replies, Deadline) ->
    receive
        {reply, Actor, Binary} ->
            <<Actual:64, 3:64, Value:64/signed-big, 0:64>> = Binary,
            true = Actual =:= Sequence,
            collect(Count - 1, Rest, Actor, Monitor, Sequence + 1,
                    [#{seq => Actual, value => Value} | Replies], Deadline);
        {'DOWN', Monitor, process, Actor, {handler_failed, Reason, Failed, Committed}} ->
            true = Failed =:= Sequence,
            #{status => <<"failure">>, reason => Reason, failed_seq => Failed,
              committed => Committed, replies => lists:reverse(Replies)};
        _ -> error(protocol_error)
    after remaining(Deadline) -> error(deadline)
    end.

remaining(Deadline) -> max(0, Deadline - erlang:monotonic_time(millisecond)).

emit(Value) -> io:put_chars([json:encode(Value), $\n]).
