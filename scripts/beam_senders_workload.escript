#!/usr/bin/env escript
%%! +S 2:2 +A 1 +SDcpu 1:1 +SDio 1 +P 1024 +Q 1024
-mode(compile).

main(Arguments) ->
    try
        need(Arguments =:= []),
        need(erlang:system_info(otp_release) =:= "29"),
        io:setopts(standard_io, [binary]),
        Input = case file:read(standard_io, 65537) of {ok, Data} -> Data; eof -> throw(invalid); _ -> error(input_read) end,
        need(byte_size(Input) =< 65536),
        Config = decode(Input),
        validate(Config),
        emit(run(Config)), halt(0)
    catch
        throw:invalid -> emit(#{version => 3, status => <<"invalid">>}), halt(2);
        _:_ -> emit(#{version => 3, status => <<"oracle_error">>}), halt(3)
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

keys(Value, Expected) -> need(is_map(Value)), need(lists:sort(maps:keys(Value)) =:= lists:sort(Expected)).
i64(Value) -> need(is_integer(Value) andalso Value >= -9223372036854775808 andalso Value =< 9223372036854775807).

validate(Config) ->
    keys(Config, [<<"version">>, <<"actors">>, <<"state_bytes">>, <<"initial">>, <<"streams">>]),
    need(maps:get(<<"version">>, Config) =:= 3),
    Count = maps:get(<<"actors">>, Config),
    need(is_integer(Count) andalso Count >= 1 andalso Count =< 100),
    need(lists:member(maps:get(<<"state_bytes">>, Config), [256, 4096, 65536])),
    Initial = maps:get(<<"initial">>, Config), i64(Initial),
    Streams = maps:get(<<"streams">>, Config),
    need(is_list(Streams) andalso length(Streams) =:= 2),
    lists:foreach(fun(Stream) ->
        need(is_list(Stream) andalso length(Stream) =< 128),
        lists:foreach(fun(Request) ->
            keys(Request, [<<"actor">>, <<"delta">>]),
            Actor = maps:get(<<"actor">>, Request),
            need(is_integer(Actor) andalso Actor >= 1 andalso Actor =< Count),
            i64(maps:get(<<"delta">>, Request))
        end, Stream)
    end, Streams),
    Bounds = lists:foldl(fun(Request, Acc) ->
        Actor = maps:get(<<"actor">>, Request), Delta = maps:get(<<"delta">>, Request),
        {Low, High} = maps:get(Actor, Acc, {Initial, Initial}),
        Acc#{Actor => {Low + min(0, Delta), High + max(0, Delta)}}
    end, #{}, lists:append(Streams)),
    maps:foreach(fun(_, {Low, High}) -> i64(Low), i64(High) end, Bounds).

options() -> [monitor, {max_heap_size, #{size => 262144, kill => true, error_logger => false, include_shared_binaries => true}}].

run(Config) ->
    Deadline = erlang:monotonic_time(millisecond) + 3000,
    Parent = self(),
    Bytes = maps:get(<<"state_bytes">>, Config), Initial = maps:get(<<"initial">>, Config),
    Count = maps:get(<<"actors">>, Config),
    Actors = [begin
        {Pid, Monitor} = spawn_opt(fun() ->
            counter(Identity, <<Initial:64/signed-big, Identity:64, 0:128, 0:((Bytes - 32) * 8)>>)
        end, options()),
        {Identity, Pid, Monitor}
    end || Identity <- lists:seq(1, Count)],
    Routes = maps:from_list([{Identity, Pid} || {Identity, Pid, _} <- Actors]),
    Streams = maps:get(<<"streams">>, Config),
    Senders = [begin
        {Pid, Monitor} = spawn_opt(fun() ->
            receive start -> ok after remaining(Deadline) -> error(deadline) end,
            Replies = batches(Stream, Sender, Routes, #{}, [], Deadline),
            Parent ! {sender_done, self(), Replies}
        end, options()),
        {Sender, Pid, Monitor}
    end || {Sender, Stream} <- lists:zip([1, 2], Streams)],
    lists:foreach(fun({_, Pid, _}) -> Pid ! start end, Senders),
    SenderMap = maps:from_list([{Pid, {Sender, Monitor}} || {Sender, Pid, Monitor} <- Senders]),
    Replies = senders(2, 2, SenderMap, #{}, #{}, [], Deadline),
    Expected = lists:foldl(fun({Sender, Stream}, Acc) ->
        lists:foldl(fun(Request, Inner) ->
            Actor = maps:get(<<"actor">>, Request),
            maps:update_with({Actor, Sender}, fun(Value) -> Value + 1 end, 1, Inner)
        end, Acc, Stream)
    end, #{}, lists:zip([1, 2], Streams)),
    lists:foreach(fun({Identity, Pid, _}) ->
        Pid ! {stop, Parent, maps:get({Identity, 1}, Expected, 0), maps:get({Identity, 2}, Expected, 0)}
    end, Actors),
    Monitors = maps:from_list([{Pid, {Identity, Monitor}} || {Identity, Pid, Monitor} <- Actors]),
    Values = stops(Count, Monitors, #{}, Deadline),
    #{version => 3, status => <<"ok">>, actors => Count, state_bytes => Bytes, message_bytes => 32,
      committed => [maps:get(Identity, Values) || Identity <- lists:seq(1, Count)], replies => Replies,
      states_verified => true, environment => #{otp => <<"29">>, senders => 2, window_per_sender => 16,
      schedulers => erlang:system_info(schedulers_online)}}.

counter(Identity, <<Committed:64/signed-big, Identity:64, First:64/signed-big, Second:64/signed-big, Padding/binary>>) ->
    receive
        {request, SenderPid, Sender, <<Sequence:64, 1:64, Delta:64/signed-big, 0:64>>} when Sender =:= 1; Sender =:= 2 ->
            Previous = case Sender of 1 -> First; 2 -> Second end,
            true = Sequence =:= Previous + 1,
            Candidate = Committed + Delta,
            true = Candidate >= -9223372036854775808 andalso Candidate =< 9223372036854775807,
            NextFirst = case Sender of 1 -> Sequence; 2 -> First end,
            NextSecond = case Sender of 1 -> Second; 2 -> Sequence end,
            State = <<Candidate:64/signed-big, Identity:64, NextFirst:64/signed-big, NextSecond:64/signed-big, Padding/binary>>,
            SenderPid ! {reply, self(), <<Sequence:64, 3:64, Candidate:64/signed-big, 0:64>>},
            counter(Identity, State);
        {stop, _Parent, First, Second} ->
            true = Padding =:= binary:copy(<<0>>, byte_size(Padding)),
            exit({completed, Committed});
        _ -> error(protocol_error)
    end.

batches([], _Sender, _Routes, _Sequences, Replies, _Deadline) -> Replies;
batches(Requests, Sender, Routes, Sequences, Replies, Deadline) ->
    {Batch, Rest} = lists:split(min(16, length(Requests)), Requests),
    {NextSequences, Pending} = lists:foldl(fun(Request, {Current, Expected}) ->
        Identity = maps:get(<<"actor">>, Request), Pid = maps:get(Identity, Routes),
        Sequence = maps:get(Identity, Current, 0) + 1, Delta = maps:get(<<"delta">>, Request),
        Pid ! {request, self(), Sender, <<Sequence:64, 1:64, Delta:64/signed-big, 0:64>>},
        {Current#{Identity => Sequence}, maps:update_with(Pid, fun(Old) -> Old ++ [{Identity, Sequence}] end, [{Identity, Sequence}], Expected)}
    end, {Sequences, #{}}, Batch),
    NextReplies = collect(Pending, Sender, Replies, Deadline),
    batches(Rest, Sender, Routes, NextSequences, NextReplies, Deadline).

collect(Pending, _Sender, Replies, _Deadline) when map_size(Pending) =:= 0 -> Replies;
collect(Pending, Sender, Replies, Deadline) ->
    receive
        {reply, Pid, <<Sequence:64, 3:64, Value:64/signed-big, 0:64>>} ->
            [{Identity, Sequence} | Tail] = maps:get(Pid, Pending),
            Next = case Tail of [] -> maps:remove(Pid, Pending); _ -> Pending#{Pid => Tail} end,
            collect(Next, Sender, [#{sender => Sender, actor => Identity, seq => Sequence, value => Value} | Replies], Deadline);
        _ -> error(protocol_error)
    after remaining(Deadline) -> error(deadline) end.

senders(0, 0, _Monitors, _Done, _Down, Replies, _Deadline) -> Replies;
senders(Count, Alive, Monitors, Done, Down, Replies, Deadline) ->
    receive
        {sender_done, Pid, Values} ->
            true = maps:is_key(Pid, Monitors), false = maps:is_key(Pid, Done),
            senders(Count - 1, Alive, Monitors, Done#{Pid => true}, Down, Values ++ Replies, Deadline);
        {'DOWN', Monitor, process, Pid, normal} ->
            {_, Monitor} = maps:get(Pid, Monitors), false = maps:is_key(Pid, Down),
            true = maps:is_key(Pid, Done),
            senders(Count, Alive - 1, Monitors, Done, Down#{Pid => true}, Replies, Deadline);
        _ -> error(protocol_error)
    after remaining(Deadline) -> error(deadline) end.

stops(0, _Monitors, Values, _Deadline) -> Values;
stops(Count, Monitors, Values, Deadline) ->
    receive
        {'DOWN', Monitor, process, Pid, {completed, Committed}} ->
            {Identity, Monitor} = maps:get(Pid, Monitors), false = maps:is_key(Identity, Values),
            stops(Count - 1, Monitors, Values#{Identity => Committed}, Deadline);
        _ -> error(protocol_error)
    after remaining(Deadline) -> error(deadline) end.

remaining(Deadline) -> max(0, Deadline - erlang:monotonic_time(millisecond)).
emit(Value) -> io:put_chars([json:encode(Value), $\n]).
