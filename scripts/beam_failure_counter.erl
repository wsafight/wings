-module(beam_failure_counter).
-export([decode/1, validate/1, load_gate/0, run/1, epoch/2, epoch/3, remaining/1, retired/1]).

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
    need(maps:get(<<"version">>, Config) =:= 9),
    Count = maps:get(<<"actors">>, Config),
    need(is_integer(Count) andalso Count >= 1 andalso Count =< 100),
    need(lists:member(maps:get(<<"state_bytes">>, Config), [256, 4096, 65536])),
    Initial = maps:get(<<"initial">>, Config), i64(Initial),
    Streams = maps:get(<<"streams">>, Config),
    need(is_list(Streams) andalso length(Streams) =:= 2),
    lists:foreach(fun(Stream) ->
        need(is_list(Stream) andalso length(Stream) =< 128),
        lists:foreach(fun(Request) ->
            keys(Request, [<<"actor">>, <<"delta">>, <<"fail">>]),
            Actor = maps:get(<<"actor">>, Request),
            need(is_integer(Actor) andalso Actor >= 1 andalso Actor =< Count),
            i64(maps:get(<<"delta">>, Request)),
            need(is_boolean(maps:get(<<"fail">>, Request)))
        end, Stream)
    end, Streams),
    Bounds = lists:foldl(fun(Request, Acc) ->
        Actor = maps:get(<<"actor">>, Request), Delta = maps:get(<<"delta">>, Request),
        {Low, High} = maps:get(Actor, Acc, {Initial, Initial}),
        Acc#{Actor => {Low + min(0, Delta), High + max(0, Delta)}}
    end, #{}, lists:append(Streams)),
    maps:foreach(fun(_, {Low, High}) -> i64(Low), i64(High) end, Bounds).

options() -> [monitor, {max_heap_size, #{size => 262144, kill => true, error_logger => false, include_shared_binaries => true}}].

load_gate() ->
    Path = filename:join(filename:dirname(escript:script_name()), "beam_bounded_mailbox.erl"),
    {ok, beam_bounded_mailbox, Binary, []} = compile:file(Path, [binary, warnings_as_errors, return_errors, return_warnings]),
    {module, beam_bounded_mailbox} = code:load_binary(beam_bounded_mailbox, Path, Binary).

run(Config) ->
    {Result, _, _} = epoch(Config, []), Result.

epoch(Config, Previous) ->
    epoch(Config, Previous, erlang:monotonic_time(millisecond) + 3000).

epoch(Config, Previous, Deadline) ->
    Parent = self(), Count = maps:get(<<"actors">>, Config),
    Bytes = maps:get(<<"state_bytes">>, Config), Initial = maps:get(<<"initial">>, Config),
    Actors = [begin
        {Pid, Monitor} = spawn_opt(fun() ->
            State = <<Initial:64/signed-big, Identity:64, 0:128, 0:((Bytes - 32) * 8)>>,
            receive {initialize, Gate} ->
                Parent ! {ready, self()}, counter(Identity, Gate, State)
            after remaining(Deadline) -> error(deadline) end
        end, options()),
        Gate = beam_bounded_mailbox:new(Pid, 32),
        Pid ! {initialize, Gate},
        {Identity, Pid, Monitor, Gate}
    end || Identity <- lists:seq(1, Count)],
    Routes = maps:from_list([{Identity, {Pid, Gate}} || {Identity, Pid, _, Gate} <- Actors]),
    ActorMap = maps:from_list([{Pid, {Identity, Monitor}} || {Identity, Pid, Monitor, _} <- Actors]),
    ready(Count, ActorMap, #{}, Deadline),
    retired(Previous),
    PreviousPids = [Pid || {_, Pid, _, _} <- Previous],
    false = lists:any(fun({_, Pid, _, _}) -> lists:member(Pid, PreviousPids) end, Actors),
    Senders = [begin
        {Pid, Monitor} = spawn_opt(fun() ->
            Monitors = maps:from_list([{ActorPid, {Identity, erlang:monitor(process, ActorPid)}}
                                      || {Identity, {ActorPid, _}} <- maps:to_list(Routes)]),
            Parent ! {ready, self()},
            receive start -> ok after remaining(Deadline) -> error(deadline) end,
            Outcomes = batches(Stream, Sender, Routes, Monitors, #{}, #{}, [], Deadline),
            Parent ! {sender_done, self(), lists:reverse(Outcomes)}
        end, options()),
        {Sender, Pid, Monitor}
    end || {Sender, Stream} <- lists:zip([1, 2], maps:get(<<"streams">>, Config))],
    SenderMap = maps:from_list([{Pid, {Sender, Monitor}} || {Sender, Pid, Monitor} <- Senders]),
    ready(2, SenderMap, #{}, Deadline),
    lists:foreach(fun({_, Pid, _}) -> Pid ! start end, Senders),
    {Outcomes, FailedStates} = gather(2, 2, SenderMap, ActorMap, #{}, #{}, [], #{}, Deadline),
    lists:foreach(fun({Identity, Pid, _, Gate}) ->
        case maps:is_key(Identity, FailedStates) of
            false ->
                case beam_bounded_mailbox:queued(Gate) of 0 -> beam_bounded_mailbox:close(Gate); closed -> ok end,
                Pid ! stop;
            true -> closed = beam_bounded_mailbox:queued(Gate)
        end
    end, Actors),
    States = stops(Count - map_size(FailedStates), ActorMap, FailedStates, Deadline),
    lists:foreach(fun({_, _, _, Gate}) -> closed = beam_bounded_mailbox:queued(Gate) end, Actors),
    receive _ -> error(unexpected_trailing_message) after 0 -> ok end,
    remaining(Deadline),
    Failures = [#{actor => Identity, reason => <<"injected_failure">>}
                || Identity <- lists:seq(1, Count), element(1, maps:get(Identity, States)) =:= failed],
    Result = #{version => 9, status => case Failures of [] -> <<"ok">>; _ -> <<"failure">> end,
      actors => Count, state_bytes => Bytes, message_bytes => 32,
      committed => [element(2, maps:get(Identity, States)) || Identity <- lists:seq(1, Count)],
      sequences => [[element(3, maps:get(Identity, States)), element(4, maps:get(Identity, States))]
                    || Identity <- lists:seq(1, Count)],
      outcomes => Outcomes, failures => Failures, states_verified => true,
      environment => #{runtime => <<"beam">>, otp => <<"29">>, senders => 2, window_per_sender => 16,
                       schedulers => erlang:system_info(schedulers_online)}},
    retired(Actors), remaining(Deadline), {Result, Actors, Deadline}.

retired(Actors) ->
    lists:foreach(fun({_, Pid, _, Gate}) ->
        false = is_process_alive(Pid),
        closed = beam_bounded_mailbox:queued(Gate),
        {error, closed} = beam_bounded_mailbox:send(Gate, stale_probe),
        closed = beam_bounded_mailbox:queued(Gate)
    end, Actors),
    length(Actors).

counter(Identity, Gate, <<Committed:64/signed-big, Identity:64, First:64/signed-big, Second:64/signed-big, Padding/binary>>) ->
    receive
        {request, SenderPid, Sender, <<Sequence:64, Operation:64, Delta:64/signed-big, 0:64>>}
                when (Sender =:= 1 orelse Sender =:= 2) andalso (Operation =:= 1 orelse Operation =:= 2) ->
            ok = beam_bounded_mailbox:started(Gate),
            Previous = case Sender of 1 -> First; 2 -> Second end,
            true = Sequence =:= Previous + 1,
            Candidate = Committed + Delta,
            true = Candidate >= -9223372036854775808 andalso Candidate =< 9223372036854775807,
            NextFirst = case Sender of 1 -> Sequence; 2 -> First end,
            NextSecond = case Sender of 1 -> Second; 2 -> Sequence end,
            State = <<Candidate:64/signed-big, Identity:64, NextFirst:64/signed-big, NextSecond:64/signed-big, Padding/binary>>,
            case Operation of
                2 ->
                    true = Padding =:= binary:copy(<<0>>, byte_size(Padding)),
                    ok = beam_bounded_mailbox:close(Gate),
                    exit({handler_failed, Committed, First, Second});
                1 ->
                    SenderPid ! {reply, self(), <<Sequence:64, 3:64, Candidate:64/signed-big, 0:64>>},
                    counter(Identity, Gate, State)
            end;
        stop ->
            true = Padding =:= binary:copy(<<0>>, byte_size(Padding)),
            exit({completed, Committed, First, Second});
        _ -> error(protocol_error)
    end.

ready(0, _Senders, _Seen, _Deadline) -> ok;
ready(Count, Senders, Seen, Deadline) ->
    receive
        {ready, Pid} ->
            true = maps:is_key(Pid, Senders), false = maps:is_key(Pid, Seen),
            ready(Count - 1, Senders, Seen#{Pid => true}, Deadline);
        _ -> error(protocol_error)
    after remaining(Deadline) -> error(deadline) end.

row(Sender, Identity, Sequence, Status, Value) ->
    #{sender => Sender, actor => Identity, seq => Sequence, status => Status, value => Value}.

batches([], _Sender, _Routes, _Monitors, _Sequences, _Down, Outcomes, _Deadline) -> Outcomes;
batches(Requests, Sender, Routes, Monitors, Sequences, Down, Outcomes, Deadline) ->
    {Batch, Rest} = lists:split(min(16, length(Requests)), Requests),
    {NextSequences, Pending, Admissions} = lists:foldl(fun(Request, {Current, Expected, Acc}) ->
        Identity = maps:get(<<"actor">>, Request), {Pid, Gate} = maps:get(Identity, Routes),
        Sequence = maps:get(Identity, Current, 0) + 1,
        case maps:is_key(Pid, Down) of
            true -> {Current#{Identity => Sequence}, Expected, [row(Sender, Identity, Sequence, <<"closed">>, null) | Acc]};
            false ->
                Operation = case maps:get(<<"fail">>, Request) of true -> 2; false -> 1 end,
                Delta = maps:get(<<"delta">>, Request),
                case beam_bounded_mailbox:send(Gate, {request, self(), Sender,
                                                    <<Sequence:64, Operation:64, Delta:64/signed-big, 0:64>>}) of
                    ok ->
                        Next = maps:update_with(Pid, fun(Old) -> Old ++ [{Identity, Sequence}] end,
                                                [{Identity, Sequence}], Expected),
                        {Current#{Identity => Sequence}, Next, Acc};
                    {error, closed} ->
                        {Current#{Identity => Sequence}, Expected, [row(Sender, Identity, Sequence, <<"closed">>, null) | Acc]};
                    _ -> error(unexpected_admission)
                end
        end
    end, {Sequences, #{}, Outcomes}, Batch),
    {NextDown, NextOutcomes} = collect(Pending, Sender, Monitors, Down, Admissions, Deadline),
    batches(Rest, Sender, Routes, Monitors, NextSequences, NextDown, NextOutcomes, Deadline).

collect(Pending, _Sender, _Monitors, Down, Outcomes, _Deadline) when map_size(Pending) =:= 0 -> {Down, Outcomes};
collect(Pending, Sender, Monitors, Down, Outcomes, Deadline) ->
    receive
        {reply, Pid, <<Sequence:64, 3:64, Value:64/signed-big, 0:64>>} ->
            [{Identity, Sequence} | Tail] = maps:get(Pid, Pending),
            false = maps:is_key(Pid, Down),
            Next = case Tail of [] -> maps:remove(Pid, Pending); _ -> Pending#{Pid => Tail} end,
            collect(Next, Sender, Monitors, Down, [row(Sender, Identity, Sequence, <<"reply">>, Value) | Outcomes], Deadline);
        {'DOWN', Monitor, process, Pid, {handler_failed, _, _, _}} ->
            {_, Monitor} = maps:get(Pid, Monitors), false = maps:is_key(Pid, Down),
            Lost = [row(Sender, Identity, Sequence, <<"failed">>, null)
                    || {Identity, Sequence} <- lists:reverse(maps:get(Pid, Pending, []))],
            collect(maps:remove(Pid, Pending), Sender, Monitors, Down#{Pid => true}, Lost ++ Outcomes, Deadline);
        _ -> error(protocol_error)
    after remaining(Deadline) -> error(deadline) end.

record_exit(Monitor, Pid, Reason, Actors, States) ->
    {Identity, Monitor} = maps:get(Pid, Actors), false = maps:is_key(Identity, States),
    State = case Reason of
        {handler_failed, Value, First, Second} -> {failed, Value, First, Second};
        {completed, Value, First, Second} -> {ok, Value, First, Second};
        _ -> error(unexpected_actor_failure)
    end,
    States#{Identity => State}.

gather(0, 0, _Senders, _Actors, _Done, _Down, Outcomes, States, _Deadline) -> {Outcomes, States};
gather(Count, Alive, Senders, Actors, Done, Down, Outcomes, States, Deadline) ->
    receive
        {sender_done, Pid, Values} ->
            true = maps:is_key(Pid, Senders), false = maps:is_key(Pid, Done),
            gather(Count - 1, Alive, Senders, Actors, Done#{Pid => true}, Down, Values ++ Outcomes, States, Deadline);
        {'DOWN', Monitor, process, Pid, normal} when is_map_key(Pid, Senders) ->
            {_, Monitor} = maps:get(Pid, Senders), false = maps:is_key(Pid, Down), true = maps:is_key(Pid, Done),
            gather(Count, Alive - 1, Senders, Actors, Done, Down#{Pid => true}, Outcomes, States, Deadline);
        {'DOWN', Monitor, process, Pid, Reason = {handler_failed, _, _, _}} ->
            Next = record_exit(Monitor, Pid, Reason, Actors, States),
            gather(Count, Alive, Senders, Actors, Done, Down, Outcomes, Next, Deadline);
        _ -> error(protocol_error)
    after remaining(Deadline) -> error(deadline) end.

stops(0, _Actors, States, _Deadline) -> States;
stops(Count, Actors, States, Deadline) ->
    receive
        {'DOWN', Monitor, process, Pid, Reason} ->
            Next = record_exit(Monitor, Pid, Reason, Actors, States),
            stops(Count - 1, Actors, Next, Deadline);
        _ -> error(protocol_error)
    after remaining(Deadline) -> error(deadline) end.

remaining(Deadline) ->
    Remaining = Deadline - erlang:monotonic_time(millisecond),
    case Remaining > 0 of true -> Remaining; false -> error(deadline) end.
