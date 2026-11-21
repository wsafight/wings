-module(beam_recovery_counter).
-export([start_actor/3, start_leaf/2, init/1, handle_call/3, handle_cast/2,
         handle_info/2, terminate/2, code_change/3, epoch/4, remaining/1]).

remaining(Deadline) ->
    case Deadline - erlang:monotonic_time(millisecond) of
        Value when Value > 0 -> Value;
        _ -> error(deadline)
    end.

call(Broker, Request, Deadline) -> gen_server:call(Broker, Request, remaining(Deadline)).

start_actor(Parent, Capacity, Deadline) ->
    gen_server:start_link(?MODULE, {actor, Parent, Capacity, Deadline}, []).

start_leaf(Parent, Deadline) -> supervisor:start_link(?MODULE, {leaf, Parent, Deadline}).

bound() ->
    process_flag(max_heap_size, #{size => 262144, kill => true,
                                 error_logger => false, include_shared_binaries => true}), ok.

init(root) -> bound(), {ok, {#{strategy => one_for_one, intensity => 1, period => 60}, []}};
init({leaf, Parent, Deadline}) ->
    bound(),
    Child = #{id => target, start => {?MODULE, start_actor, [Parent, 16, Deadline]},
              restart => transient, shutdown => 1000, type => worker},
    {ok, {#{strategy => one_for_one, intensity => 1, period => 60}, [Child]}};
init(broker) -> bound(), {ok, #{}};
init({actor, Parent, Capacity, Deadline}) ->
    bound(),
    Gate = beam_bounded_mailbox:new(self(), Capacity),
    State = <<0:32768>>,
    Parent ! {ready, self(), Gate, State},
    {ok, {Parent, Gate, State, Deadline}}.

handle_call({submit, Gate = {bounded_mailbox, Actor, _, _}, Operation, Payload, Expires}, _From, Slots)
        when is_map(Slots), map_size(Slots) < 17 ->
    Ticket = erlang:monitor(process, Actor),
    case beam_bounded_mailbox:send(Gate, {request, self(), Ticket, Operation, Payload}) of
        ok ->
            Now = erlang:monotonic_time(millisecond),
            Terminal = case Expires of
                infinity -> pending;
                _ when Expires =< Now -> timed_out
            end,
            Slot = #{actor => Actor, terminal => Terminal, consumed => false, published => false},
            {reply, {ok, Ticket}, Slots#{Ticket => Slot}};
        Error ->
            erlang:demonitor(Ticket, [flush]),
            {reply, Error, Slots}
    end;
handle_call({submit, _, _, _, _}, _From, Slots) when is_map(Slots) ->
    {reply, {error, requests_full}, Slots};
handle_call({cancel, Ticket}, _From, Slots) when is_map(Slots) ->
    Next = case maps:find(Ticket, Slots) of
        {ok, Slot = #{terminal := pending}} -> Slots#{Ticket => Slot#{terminal => cancelled}};
        _ -> Slots
    end,
    {reply, ok, Next};
handle_call({take, Ticket}, _From, Slots) when is_map(Slots) ->
    case maps:find(Ticket, Slots) of
        error -> {reply, missing, Slots};
        {ok, #{consumed := true}} -> {reply, missing, Slots};
        {ok, #{terminal := pending}} -> {reply, pending, Slots};
        {ok, Slot = #{terminal := Terminal}} ->
            {reply, Terminal, retain(Ticket, Slot#{consumed => true}, Slots)}
    end;
handle_call(snapshot, _From, Slots) when is_map(Slots) ->
    {reply, map_size(Slots), Slots};
handle_call(Request, _From, State) -> error({unexpected_call, Request, State}).

retain(Ticket, #{consumed := true, published := true}, Slots) -> maps:remove(Ticket, Slots);
retain(Ticket, Slot, Slots) -> Slots#{Ticket => Slot}.

publish(Ticket, Result, Slots) ->
    Slot = maps:get(Ticket, Slots),
    false = maps:get(published, Slot),
    Terminal = case maps:get(terminal, Slot) of pending -> Result; Previous -> Previous end,
    retain(Ticket, Slot#{published => true, terminal => Terminal}, Slots).

handle_cast(Request, State) -> error({unexpected_cast, Request, State}).

handle_info({published, Ticket, Value}, Slots) when is_map(Slots) ->
    erlang:demonitor(Ticket, [flush]),
    {noreply, publish(Ticket, {reply, Value}, Slots)};
handle_info({'DOWN', Ticket, process, Actor, _Reason}, Slots) when is_map(Slots) ->
    #{actor := Actor} = maps:get(Ticket, Slots),
    {noreply, publish(Ticket, failed, Slots)};
handle_info(hold, Actor = {Parent, Gate, _, Deadline}) ->
    ok = beam_bounded_mailbox:started(Gate),
    Parent ! {holding, self()},
    receive release -> {noreply, Actor} after remaining(Deadline) -> error(deadline) end;
handle_info({request, Broker, Ticket, add, <<Delta:64/signed-big>>}, {Parent, Gate, State, Deadline}) ->
    ok = beam_bounded_mailbox:started(Gate),
    <<Old:64/signed-big, Tail/binary>> = State,
    Value = Old + Delta,
    true = Value >= -9223372036854775808 andalso Value =< 9223372036854775807,
    Next = <<Value:64/signed-big, Tail/binary>>,
    Broker ! {published, Ticket, Value},
    {noreply, {Parent, Gate, Next, Deadline}};
handle_info({request, _Broker, _Ticket, fail, <<>>}, {Parent, Gate, State, _Deadline}) ->
    ok = beam_bounded_mailbox:started(Gate),
    <<Zero:64/signed-big, Tail/binary>> = State,
    Candidate = <<99:64/signed-big, Tail/binary>>,
    try 1 div Zero of
        _ -> error(unexpected_success)
    catch error:badarith:Stack ->
        Parent ! {handler_failed, self(), division_by_zero, State, byte_size(Candidate)},
        erlang:raise(error, badarith, Stack)
    end;
handle_info({snapshot, From, Ref}, Actor = {_, _, State, _}) ->
    From ! {snapshot, Ref, State}, {noreply, Actor};
handle_info(Request, State) -> error({unexpected_info, Request, State}).

terminate(_, _) -> ok.
code_change(_, State, _) -> {ok, State}.

ready(Deadline) ->
    receive {ready, Actor, Gate, <<0:32768>>} -> {Actor, Gate}
    after remaining(Deadline) -> error(deadline) end.

take(Broker, Ticket, Deadline) ->
    case call(Broker, {take, Ticket}, Deadline) of
        pending -> receive after 1 -> take(Broker, Ticket, Deadline) end;
        Value -> Value
    end.

consume(Broker, Ticket, Deadline) ->
    Status = take(Broker, Ticket, Deadline),
    missing = call(Broker, {take, Ticket}, Deadline),
    Status.

empty(Broker, Deadline) ->
    case call(Broker, snapshot, Deadline) of
        0 -> ok;
        _ -> receive after 1 -> empty(Broker, Deadline) end
    end.

child_gone(Root, Child, Deadline) ->
    case lists:keymember(Child, 1, supervisor:which_children(Root)) of
        false -> ok;
        true -> remaining(Deadline), receive after 1 -> child_gone(Root, Child, Deadline) end
    end.

retired(Actors, Root, Children, Broker, Deadline) ->
    lists:foreach(fun(Child) -> false = lists:keymember(Child, 1, supervisor:which_children(Root)) end, Children),
    lists:foreach(fun({Actor, Gate}) ->
        false = is_process_alive(Actor),
        closed = beam_bounded_mailbox:queued(Gate),
        Before = call(Broker, snapshot, Deadline),
        {error, closed} = call(Broker, {submit, Gate, add, <<999:64/signed-big>>, infinity}, Deadline),
        Before = call(Broker, snapshot, Deadline)
    end, Actors),
    length(Actors).

sender(Parent, Broker, Gate, Sender, Deadline) ->
    bound(),
    receive start -> ok after remaining(Deadline) -> error(deadline) end,
    {Pending, Outcomes} = lists:foldl(fun(Sequence, {Tickets, Rows}) ->
        Expires = case Sequence =< 2 of true -> erlang:monotonic_time(millisecond); false -> infinity end,
        Delta = (Sender - 1) * 8 + Sequence,
        {ok, Ticket} = call(Broker, {submit, Gate, add, <<Delta:64/signed-big>>, Expires}, Deadline),
        case Sequence =< 4 of
            true ->
                case Sequence > 2 of true -> ok = call(Broker, {cancel, Ticket}, Deadline); false -> ok end,
                Status = consume(Broker, Ticket, Deadline),
                Expected = case Sequence =< 2 of true -> timed_out; false -> cancelled end,
                Expected = Status,
                {Tickets, [row(Sender, Sequence, Status) | Rows]};
            false -> {[{Sequence, Ticket} | Tickets], Rows}
        end
    end, {[], []}, lists:seq(1, 8)),
    Parent ! {queued, self()},
    receive cancel -> ok after remaining(Deadline) -> error(deadline) end,
    Final = lists:foldl(fun({Sequence, Ticket}, Rows) ->
        ok = call(Broker, {cancel, Ticket}, Deadline),
        Status = consume(Broker, Ticket, Deadline),
        true = lists:member(Status, [cancelled, failed]),
        [row(Sender, Sequence, Status) | Rows]
    end, Outcomes, Pending),
    Parent ! {sender_done, self(), Final}.

row(Sender, Sequence, Status) ->
    #{sender => Sender, seq => Sequence, status => atom_to_binary(Status), consumed_once => true}.

epoch(Root, Broker, {Previous, PreviousChildren}, Deadline) ->
    Parent = self(),
    [] = supervisor:which_children(Root),
    LeafId = make_ref(), HealthyId = make_ref(),
    {ok, Leaf} = supervisor:start_child(Root, #{id => LeafId, start => {?MODULE, start_leaf, [Parent, Deadline]},
        restart => temporary, type => supervisor, shutdown => 1000}),
    LeafMonitor = erlang:monitor(process, Leaf),
    {Target, Gate} = ready(Deadline),
    {ok, Healthy} = supervisor:start_child(Root, #{id => HealthyId,
        start => {?MODULE, start_actor, [Parent, 1, Deadline]}, restart => temporary, shutdown => 1000}),
    {Healthy, HealthyGate} = ready(Deadline),
    PreviousChecks = retired(Previous, Root, PreviousChildren, Broker, Deadline),
    ok = beam_bounded_mailbox:send(Gate, hold),
    receive {holding, Target} -> ok after remaining(Deadline) -> error(deadline) end,
    Senders = [spawn_monitor(fun() -> sender(Parent, Broker, Gate, Sender, Deadline) end) || Sender <- [1, 2]],
    lists:foreach(fun({Pid, _}) -> Pid ! start end, Senders),
    lists:foreach(fun({Pid, _}) ->
        receive {queued, Pid} -> ok after remaining(Deadline) -> error(deadline) end
    end, Senders),
    Before = #{queued => beam_bounded_mailbox:queued(Gate), requests => call(Broker, snapshot, Deadline)},
    #{queued := 16, requests := 16} = Before,
    {error, mailbox_full} = call(Broker, {submit, Gate, add, <<999:64/signed-big>>, infinity}, Deadline),
    Before = #{queued => beam_bounded_mailbox:queued(Gate), requests => call(Broker, snapshot, Deadline)},
    {ok, Health} = call(Broker, {submit, HealthyGate, add, <<7:64/signed-big>>, infinity}, Deadline),
    {reply, 7} = consume(Broker, Health, Deadline),
    lists:foreach(fun({Pid, _}) -> Pid ! cancel end, Senders),
    ok = beam_bounded_mailbox:close(Gate),
    ok = supervisor:terminate_child(Leaf, target),
    [{target, undefined, worker, _}] = supervisor:which_children(Leaf),
    Outcomes = lists:append([begin
        Values = receive {sender_done, Pid, Rows} -> Rows after remaining(Deadline) -> error(deadline) end,
        receive {'DOWN', Monitor, process, Pid, normal} -> ok after remaining(Deadline) -> error(deadline) end,
        Values
    end || {Pid, Monitor} <- Senders]),
    empty(Broker, Deadline),
    {ok, First} = supervisor:restart_child(Leaf, target),
    {First, FirstGate} = ready(Deadline),
    FirstMonitor = erlang:monitor(process, First),
    FirstResult = failure(Broker, First, FirstGate, FirstMonitor, 1, Deadline),
    {Second, SecondGate} = ready(Deadline),
    true = First =/= Second,
    SecondMonitor = erlang:monitor(process, Second),
    SecondResult = failure(Broker, Second, SecondGate, SecondMonitor, 2, Deadline),
    receive {'DOWN', LeafMonitor, process, Leaf, shutdown} -> ok after remaining(Deadline) -> error(deadline) end,
    child_gone(Root, LeafId, Deadline),
    Snapshot = make_ref(), Healthy ! {snapshot, self(), Snapshot},
    receive {snapshot, Snapshot, <<7:64/signed-big, 0:32704>>} -> ok after remaining(Deadline) -> error(deadline) end,
    ok = beam_bounded_mailbox:close(HealthyGate),
    ok = supervisor:terminate_child(Root, HealthyId),
    child_gone(Root, HealthyId, Deadline),
    empty(Broker, Deadline),
    Actors = [{Target, Gate}, {Healthy, HealthyGate}, {First, FirstGate}, {Second, SecondGate}],
    Children = [LeafId, HealthyId],
    Checks = PreviousChecks + retired(Actors, Root, Children, Broker, Deadline),
    Result = #{state_bytes => 4096, queued => 16, retained_request_slots => 16, mailbox_full => true,
        before_rejected => Before, after_rejected => Before, healthy_committed => 7, stop_restarted => false,
        outcomes => Outcomes, failures => [FirstResult, SecondResult]},
    {Result, {Actors, Children}, Checks}.

failure(Broker, Actor, Gate, Monitor, Attempt, Deadline) ->
    {ok, Ticket} = call(Broker, {submit, Gate, fail, <<>>, infinity}, Deadline),
    ok = call(Broker, {cancel, Ticket}, Deadline),
    Status = consume(Broker, Ticket, Deadline),
    true = lists:member(Status, [cancelled, failed]),
    receive {handler_failed, Actor, division_by_zero, <<0:32768>>, 4096} -> ok
    after remaining(Deadline) -> error(deadline) end,
    receive {'DOWN', Monitor, process, Actor, {badarith, Stack}} when is_list(Stack) -> ok
    after remaining(Deadline) -> error(deadline) end,
    ok = beam_bounded_mailbox:close(Gate),
    empty(Broker, Deadline),
    #{attempt => Attempt, status => atom_to_binary(Status), handler => <<"fail">>,
      reason => <<"division_by_zero">>, state_bytes => 4096, committed => 0,
      restart => case Attempt of 1 -> <<"restarted">>; 2 -> <<"intensity_exceeded">> end,
      next_initial => case Attempt of 1 -> 0; 2 -> null end}.
