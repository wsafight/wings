#!/usr/bin/env escript
%%! +S 2:2 +A 1 +SDcpu 1:1 +SDio 1 +P 1024 +Q 1024
-mode(compile).

main(Arguments) ->
    try
        need(erlang:system_info(otp_release) =:= "29"),
        load_gate(),
        Result = case Arguments of
            ["--gate-tests"] -> gate_tests();
            [Text] ->
                Capacity = try list_to_integer(Text) catch _:_ -> throw(invalid) end,
                need(lists:member(Capacity, [1, 100, 256])),
                run(Capacity);
            _ -> throw(invalid)
        end,
        emit(Result), halt(0)
    catch
        throw:invalid -> emit(#{version => 4, status => <<"invalid">>}), halt(2);
        _:_ -> emit(#{version => 4, status => <<"oracle_error">>}), halt(3)
    end.

need(true) -> ok;
need(false) -> throw(invalid).

load_gate() ->
    Path = filename:join(filename:dirname(escript:script_name()), "beam_bounded_mailbox.erl"),
    {ok, beam_bounded_mailbox, Binary, []} = compile:file(Path, [binary, warnings_as_errors, return_errors, return_warnings]),
    {module, beam_bounded_mailbox} = code:load_binary(beam_bounded_mailbox, Path, Binary).

options() -> [monitor, {max_heap_size, #{size => 262144, kill => true, error_logger => false, include_shared_binaries => true}}].

remaining(Deadline) -> max(0, Deadline - erlang:monotonic_time(millisecond)).

run(Capacity) ->
    Deadline = erlang:monotonic_time(millisecond) + 3000,
    Parent = self(),
    {Busy, BusyMonitor} = spawn_opt(fun() -> initialize(Parent) end, options()),
    {Healthy, HealthyMonitor} = spawn_opt(fun() -> initialize(Parent) end, options()),
    BusyGate = beam_bounded_mailbox:new(Busy, Capacity),
    HealthyGate = beam_bounded_mailbox:new(Healthy, 1),
    Busy ! {initialize, BusyGate}, Healthy ! {initialize, HealthyGate},
    ready([Busy, Healthy], Deadline),
    ok = beam_bounded_mailbox:send(BusyGate, {request, Parent, <<0:64, 2:64, 0:128>>}),
    receive {holding, Busy} -> ok after remaining(Deadline) -> error(deadline) end,
    lists:foreach(fun(Sequence) ->
        ok = beam_bounded_mailbox:send(BusyGate, {request, Parent, <<Sequence:64, 1:64, 1:64/signed-big, 0:64>>})
    end, lists:seq(1, Capacity)),
    Capacity = beam_bounded_mailbox:queued(BusyGate),
    {error, mailbox_full} = beam_bounded_mailbox:send(BusyGate, {request, Parent, <<999:64, 1:64, 1:64/signed-big, 0:64>>}),
    Capacity = beam_bounded_mailbox:queued(BusyGate),
    ok = beam_bounded_mailbox:send(HealthyGate, {notify, <<1:64, 1:64, 7:64/signed-big, 0:64>>}),
    Healthy ! {inspect, Parent},
    receive {snapshot, Healthy, 7} -> ok after remaining(Deadline) -> error(deadline) end,
    ok = beam_bounded_mailbox:close(BusyGate),
    {error, closed} = beam_bounded_mailbox:send(BusyGate, stale),
    receive {'DOWN', BusyMonitor, process, Busy, {closed, 0, Capacity}} -> ok after remaining(Deadline) -> error(deadline) end,
    closed = beam_bounded_mailbox:queued(BusyGate),
    ok = beam_bounded_mailbox:close(HealthyGate), Healthy ! {stop, Parent},
    receive {'DOWN', HealthyMonitor, process, Healthy, {completed, 7}} -> ok after remaining(Deadline) -> error(deadline) end,
    receive _ -> error(unexpected_reply) after 0 -> ok end,
    #{version => 4, status => <<"ok">>, state_bytes => 4096, mailbox_capacity => Capacity,
      accepted => Capacity, rejected_full => 1, failed => Capacity, healthy_value => 7,
      initial_value => 0, queue_released => true, stale_rejected => true,
      environment => #{otp => <<"29">>, schedulers => erlang:system_info(schedulers_online)}}.

initialize(Parent) ->
    receive {initialize, Gate} ->
        State = binary:copy(<<0>>, 4096), Parent ! {ready, self()}, counter(Parent, Gate, State)
    after 3000 -> error(deadline) end.

ready([], _Deadline) -> ok;
ready(Pending, Deadline) ->
    receive {ready, Pid} ->
        true = lists:member(Pid, Pending), ready(lists:delete(Pid, Pending), Deadline)
    after remaining(Deadline) -> error(deadline) end.

counter(Parent, Gate, <<Committed:64/signed-big, Padding/binary>> = State) ->
    receive
        {notify, <<_Sequence:64, 1:64, Delta:64/signed-big, 0:64>>} ->
            ok = beam_bounded_mailbox:started(Gate),
            Candidate = Committed + Delta,
            true = Candidate >= -9223372036854775808 andalso Candidate =< 9223372036854775807,
            counter(Parent, Gate, <<Candidate:64/signed-big, Padding/binary>>);
        {inspect, Parent} -> Parent ! {snapshot, self(), Committed}, counter(Parent, Gate, State);
        {request, Parent, <<Sequence:64, Operation:64, Delta:64/signed-big, 0:64>>} ->
            ok = beam_bounded_mailbox:started(Gate),
            case Operation of
                2 -> Parent ! {holding, self()}, hold(Gate, State);
                1 ->
                    Candidate = Committed + Delta,
                    true = Candidate >= -9223372036854775808 andalso Candidate =< 9223372036854775807,
                    Next = <<Candidate:64/signed-big, Padding/binary>>,
                    Parent ! {reply, self(), <<Sequence:64, 3:64, Candidate:64/signed-big, 0:64>>},
                    counter(Parent, Gate, Next)
            end;
        {stop, Parent} ->
            true = byte_size(State) =:= 4096,
            true = Padding =:= binary:copy(<<0>>, byte_size(Padding)),
            exit({completed, Committed});
        _ -> error(protocol_error)
    end.

hold(Gate, <<Committed:64/signed-big, Padding/binary>> = State) ->
    case beam_bounded_mailbox:queued(Gate) of
        closed ->
            true = byte_size(State) =:= 4096,
            true = Padding =:= binary:copy(<<0>>, byte_size(Padding)),
            {message_queue_len, Queued} = process_info(self(), message_queue_len),
            exit({closed, Committed, Queued});
        _ -> hold(Gate, State)
    end.

gate_tests() ->
    Deadline = erlang:monotonic_time(millisecond) + 3000,
    lists:foreach(fun(Capacity) ->
        try beam_bounded_mailbox:new(self(), Capacity) of _ -> error(accepted_invalid_capacity)
        catch error:badarg -> ok end
    end, [0, 257, -1, 1.0, true]),
    lists:foreach(fun(Capacity) ->
        Gate = beam_bounded_mailbox:new(self(), Capacity),
        {error, not_queued} = beam_bounded_mailbox:started(Gate),
        lists:foreach(fun(Sequence) -> ok = beam_bounded_mailbox:send(Gate, {direct, Sequence}) end, lists:seq(1, Capacity)),
        {error, mailbox_full} = beam_bounded_mailbox:send(Gate, unexpected),
        lists:foreach(fun(Sequence) ->
            receive {direct, Sequence} -> ok = beam_bounded_mailbox:started(Gate) after remaining(Deadline) -> error(deadline) end
        end, lists:seq(1, Capacity)),
        0 = beam_bounded_mailbox:queued(Gate),
        ok = beam_bounded_mailbox:send(Gate, reused),
        receive reused -> ok = beam_bounded_mailbox:started(Gate) after remaining(Deadline) -> error(deadline) end,
        race_gate(Capacity, Deadline),
        ok = beam_bounded_mailbox:send(Gate, late),
        ok = beam_bounded_mailbox:close(Gate), ok = beam_bounded_mailbox:close(Gate),
        {error, closed} = beam_bounded_mailbox:send(Gate, unexpected),
        receive late -> {error, closed} = beam_bounded_mailbox:started(Gate) after remaining(Deadline) -> error(deadline) end,
        closed = beam_bounded_mailbox:queued(Gate)
    end, [1, 100, 256]),
    close_race(Deadline),
    #{version => 4, status => <<"ok">>, gate_tests => 3, senders => 2}.

close_race(Deadline) ->
    Gate = beam_bounded_mailbox:new(self(), 256), Parent = self(),
    {Sender, Monitor} = spawn_opt(fun() ->
        receive start -> ok after remaining(Deadline) -> error(deadline) end,
        {Accepted, Closed} = lists:foldl(fun(Sequence, {Total, Rejected}) ->
            case beam_bounded_mailbox:send(Gate, {closing, self(), Sequence}) of
                ok -> {Total + 1, Rejected};
                {error, closed} -> {Total, Rejected + 1}
            end
        end, {0, 0}, lists:seq(1, 256)),
        Parent ! {closed_race, self(), Accepted, Closed}
    end, options()),
    Sender ! start, beam_bounded_mailbox:close(Gate),
    Accepted = receive {closed_race, Sender, Total, Rejected} ->
        256 = Total + Rejected, Total
    after remaining(Deadline) -> error(deadline) end,
    receive {'DOWN', Monitor, process, Sender, normal} -> ok after remaining(Deadline) -> error(deadline) end,
    lists:foreach(fun(Sequence) ->
        receive {closing, Sender, Sequence} -> {error, closed} = beam_bounded_mailbox:started(Gate)
        after remaining(Deadline) -> error(deadline) end
    end, lists:seq(1, Accepted)),
    closed = beam_bounded_mailbox:queued(Gate),
    {error, closed} = beam_bounded_mailbox:send(Gate, unexpected),
    receive _ -> error(unexpected_message) after 0 -> ok end.

race_gate(Capacity, Deadline) ->
    Gate = beam_bounded_mailbox:new(self(), Capacity), Parent = self(),
    Senders = [begin
        {Pid, Monitor} = spawn_opt(fun() ->
            {error, foreign_actor} = beam_bounded_mailbox:started(Gate),
            receive start -> ok after remaining(Deadline) -> error(deadline) end,
            Accepted = lists:foldl(fun(Sequence, Total) ->
                case beam_bounded_mailbox:send(Gate, {raced, self(), Sequence}) of
                    ok -> Total + 1;
                    {error, mailbox_full} -> Total
                end
            end, 0, lists:seq(1, Capacity)),
            Parent ! {attempted, self(), Accepted}
        end, options()), {Pid, Monitor}
    end || _ <- [1, 2]],
    lists:foreach(fun({Pid, _}) -> Pid ! start end, Senders),
    Counts = attempts(Senders, #{}, [], Deadline),
    Capacity = lists:sum(Counts), Capacity = beam_bounded_mailbox:queued(Gate),
    drain(Capacity, Gate, #{}, Deadline),
    0 = beam_bounded_mailbox:queued(Gate), beam_bounded_mailbox:close(Gate).

attempts([], _Done, Counts, _Deadline) -> Counts;
attempts(Pending, Done, Counts, Deadline) ->
    receive
        {attempted, Pid, Count} ->
            true = lists:keymember(Pid, 1, Pending), false = maps:is_key(Pid, Done),
            attempts(Pending, Done#{Pid => Count}, Counts, Deadline);
        {'DOWN', Monitor, process, Pid, normal} ->
            {Pid, Monitor} = lists:keyfind(Pid, 1, Pending), Count = maps:get(Pid, Done),
            attempts(lists:keydelete(Pid, 1, Pending), Done, [Count | Counts], Deadline)
    after remaining(Deadline) -> error(deadline) end.

drain(0, _Gate, _Sequences, _Deadline) -> ok;
drain(Count, Gate, Sequences, Deadline) ->
    receive {raced, Pid, Sequence} ->
        Previous = maps:get(Pid, Sequences, 0), true = Sequence =:= Previous + 1,
        ok = beam_bounded_mailbox:started(Gate),
        drain(Count - 1, Gate, Sequences#{Pid => Sequence}, Deadline)
    after remaining(Deadline) -> error(deadline) end.

emit(Value) -> io:put_chars([json:encode(Value), $\n]).
