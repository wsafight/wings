#!/usr/bin/env escript
%%! +S 1:1 +A 1 +SDcpu 1:1 +SDio 1 +P 1024 +Q 1024
-mode(compile).

main(Arguments) ->
    try
        Count = case Arguments of ["10"] -> 100; ["30"] -> 300; _ -> throw(invalid) end,
        need(erlang:system_info(otp_release) =:= "29"),
        load_gate(),
        emit(run(Count)), halt(0)
    catch
        throw:invalid -> emit(#{version => 7, status => <<"invalid">>}), halt(2);
        _:_ -> emit(#{version => 7, status => <<"oracle_error">>}), halt(3)
    end.

need(true) -> ok;
need(false) -> throw(invalid).

load_gate() ->
    Path = filename:join(filename:dirname(escript:script_name()), "beam_bounded_mailbox.erl"),
    {ok, beam_bounded_mailbox, Binary, []} = compile:file(Path, [binary, warnings_as_errors, return_errors, return_warnings]),
    {module, beam_bounded_mailbox} = code:load_binary(beam_bounded_mailbox, Path, Binary).

remaining(Deadline) ->
    Left = Deadline - erlang:monotonic_time(millisecond),
    case Left > 0 of true -> Left; false -> error(stage_deadline) end.

run(Count) ->
    Deadline = erlang:monotonic_time(millisecond) + 3000,
    Parent = self(),
    Entries = [begin
        {Pid, Monitor} = spawn_opt(fun() -> initialize(Parent, Identity) end,
            [monitor, {max_heap_size, #{size => 262144, kill => true, error_logger => false, include_shared_binaries => true}}]),
        Gate = beam_bounded_mailbox:new(Pid, 1),
        Pid ! {initialize, Gate},
        {Identity, Pid, Monitor, Gate}
    end || Identity <- lists:seq(1, 101)],
    lists:foreach(fun({Identity, Pid, _, _}) ->
        receive {ready, Pid, Identity} -> ok after remaining(Deadline) -> error(stage_deadline) end
    end, Entries),
    Routes = maps:from_list([{Identity, {Pid, Gate}} || {Identity, Pid, _, Gate} <- Entries]),
    Initial = #{next => 0, count => Count, pending => #{}, hot_pending => none, hot => [], hot_count => 0,
        values => #{}, received => #{}, routes => Routes, samples => array:new(Count, {fixed, true}),
        start => erlang:monotonic_time(nanosecond), deadline => Deadline},
    Result = loop(Initial),
    Values = maps:get(values, Result),
    lists:foreach(fun({_, Pid, _, Gate}) ->
        0 = beam_bounded_mailbox:queued(Gate), ok = beam_bounded_mailbox:close(Gate), Pid ! {stop, Parent}
    end, Entries),
    lists:foreach(fun({Identity, Pid, Monitor, _}) ->
        Expected = maps:get(Identity, Values, 0),
        receive {'DOWN', Monitor, process, Pid, {completed, Expected}} -> ok
        after remaining(Deadline) -> error(stage_deadline) end
    end, Entries),
    receive _ -> error(trailing_message) after 0 -> ok end,
    #{version => 7, status => <<"ok">>, mode => <<"smoke">>, actors => 101, state_bytes => 256,
      mailbox_capacity => 1, pending_capacity => 255, deadline_ns => 100000000, message_bytes => 32,
      samples => [tuple_to_list(Sample) || Sample <- array:to_list(maps:get(samples, Result))],
      hot => lists:reverse(maps:get(hot, Result)), states_verified => true,
      committed => [maps:get(Identity, Values, 0) || Identity <- lists:seq(1, 101)],
      environment => #{runtime => <<"beam">>, otp => <<"29">>, schedulers => erlang:system_info(schedulers_online)}}.

initialize(Parent, Identity) ->
    State = <<0:64/signed-big, Identity:64, 0:1920>>,
    receive {initialize, Gate} -> Parent ! {ready, self(), Identity}, counter(Parent, Identity, Gate, State) end.

counter(Parent, Identity, Gate, <<Committed:64/signed-big, Identity:64, Padding/binary>>) ->
    receive
        {request, Parent, <<Sequence:64, Operation:64, Delta:64/signed-big, 0:64>>} ->
            ok = beam_bounded_mailbox:started(Gate),
            Reply = case {Identity, Operation, Delta} of
                {1, 2, 0} -> million(0);
                {Cold, 1, 1} when Cold >= 2 -> Committed + 1;
                _ -> error(protocol)
            end,
            Candidate = Committed + 1,
            true = Candidate =< 9223372036854775807,
            CandidateState = <<Candidate:64/signed-big, Identity:64, Padding/binary>>,
            Parent ! {reply, self(), <<Sequence:64, 3:64, Reply:64/signed-big, 0:64>>},
            counter(Parent, Identity, Gate, CandidateState);
        {stop, Parent} ->
            true = Padding =:= binary:copy(<<0>>, 240), exit({completed, Committed});
        _ -> error(protocol)
    end.

ten(Value) ->
    First = checked(Value), Second = checked(First), Third = checked(Second), Fourth = checked(Third),
    Fifth = checked(Fourth), Sixth = checked(Fifth), Seventh = checked(Sixth), Eighth = checked(Seventh),
    Ninth = checked(Eighth), checked(Ninth).
hundred(Value) -> ten(ten(ten(ten(ten(ten(ten(ten(ten(ten(Value)))))))))).
thousand(Value) -> hundred(hundred(hundred(hundred(hundred(hundred(hundred(hundred(hundred(hundred(Value)))))))))).
ten_thousand(Value) -> thousand(thousand(thousand(thousand(thousand(thousand(thousand(thousand(thousand(thousand(Value)))))))))).
hundred_thousand(Value) -> ten_thousand(ten_thousand(ten_thousand(ten_thousand(ten_thousand(ten_thousand(ten_thousand(ten_thousand(ten_thousand(ten_thousand(Value)))))))))).
million(Value) -> hundred_thousand(hundred_thousand(hundred_thousand(hundred_thousand(hundred_thousand(hundred_thousand(hundred_thousand(hundred_thousand(hundred_thousand(hundred_thousand(Value)))))))))).
checked(Value) ->
    Candidate = Value + 1,
    true = Candidate >= -9223372036854775808 andalso Candidate =< 9223372036854775807, Candidate.

elapsed(State) -> erlang:monotonic_time(nanosecond) - maps:get(start, State).
planned(Index) -> Index div 100 * 10000000.

loop(State) ->
    _ = remaining(maps:get(deadline, State)),
    Current = start_hot(collect(State, 32)),
    #{next := Next, count := Count, pending := Pending, hot_pending := Hot} = Current,
    case Next =:= Count andalso map_size(Pending) =:= 0 andalso Hot =:= none of
        true -> Current;
        false ->
            case Next < Count andalso elapsed(Current) >= planned(Next) of
                true -> loop(arrive(Current));
                false -> erlang:yield(), loop(Current)
            end
    end.

start_hot(#{next := Next, count := Count, hot_pending := none, hot_count := Completed, routes := Routes} = State) when Next < Count ->
    true = Completed < 10000,
    {_Pid, Gate} = maps:get(1, Routes),
    Sent = elapsed(State), Sequence = Completed + 1,
    ok = beam_bounded_mailbox:send(Gate, {request, self(), <<Sequence:64, 2:64, 0:128>>}),
    State#{hot_pending := {Sequence, Sent}};
start_hot(State) -> State.

arrive(#{next := Index, routes := Routes, samples := Samples, pending := Pending, values := Values} = State) ->
    Identity = 2 + Index rem 100, Sequence = Index + 1, Planned = planned(Index), Sent = elapsed(State),
    {Pid, Gate} = maps:get(Identity, Routes),
    Admission = case map_size(Pending) =:= 255 of
        true -> {error, pending_full};
        false -> beam_bounded_mailbox:send(Gate, {request, self(), <<Sequence:64, 1:64, 1:64/signed-big, 0:64>>})
    end,
    {Status, NextPending, NextValues} = case Admission of
        ok -> Value = maps:get(Identity, Values, 0) + 1,
            {<<"pending">>, Pending#{Sequence => {Identity, Pid, Value}}, Values#{Identity => Value}};
        {error, mailbox_full} -> {<<"mailbox_full">>, Pending, Values};
        {error, pending_full} -> {<<"pending_full">>, Pending, Values}
    end,
    Sample = {Sequence, Identity, Planned, Sent, Status, null, null},
    State#{next := Index + 1, pending := NextPending, values := NextValues, samples := array:set(Index, Sample, Samples)}.

collect(State, 0) -> State;
collect(#{routes := Routes, hot_pending := Hot, hot := History, hot_count := Completed, values := Values,
          pending := Pending, received := Received, samples := Samples} = State, Limit) ->
    {HotPid, _Gate} = maps:get(1, Routes),
    receive
        {reply, HotPid, <<Sequence:64, 3:64, 1000000:64/signed-big, 0:64>>} ->
            Observed = elapsed(State),
            {Sequence, Sent} = Hot,
            true = Sequence =:= Completed + 1,
            collect(State#{hot_pending := none, hot_count := Sequence, hot := [[Sequence, Sent, Observed, 1000000] | History],
                values := Values#{1 => Sequence}}, Limit - 1);
        {reply, Pid, <<Sequence:64, 3:64, Value:64/signed-big, 0:64>>} ->
            Observed = elapsed(State),
            {Identity, Pid, Value} = maps:get(Sequence, Pending),
            true = Value =:= maps:get(Identity, Received, 0) + 1,
            {Sequence, Identity, Planned, Sent, <<"pending">>, null, null} = array:get(Sequence - 1, Samples),
            Status = case Observed < Planned + 100000000 of true -> <<"reply">>; false -> <<"timed_out">> end,
            Sample = {Sequence, Identity, Planned, Sent, Status, Observed, Value},
            collect(State#{pending := maps:remove(Sequence, Pending), received := Received#{Identity => Value},
                samples := array:set(Sequence - 1, Sample, Samples)}, Limit - 1);
        _ -> error(unexpected_message)
    after 0 -> State
    end.

emit(Value) -> io:put_chars([json:encode(Value), $\n]).
