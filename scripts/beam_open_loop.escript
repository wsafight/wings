#!/usr/bin/env escript
%%! +S 2:2 +A 1 +SDcpu 1:1 +SDio 1 +P 32768 +Q 1024
-mode(compile).

main(Arguments) ->
    try
        Config = validate(Arguments),
        need(erlang:system_info(otp_release) =:= "29"),
        load_gate(),
        emit(run(Config)), halt(0)
    catch
        throw:invalid -> emit(#{version => 5, status => <<"invalid">>}), halt(2);
        _:_ -> emit(#{version => 5, status => <<"oracle_error">>}), halt(3)
    end.

need(true) -> ok;
need(false) -> throw(invalid).

number(Text) ->
    need(Text =/= [] andalso lists:all(fun(Digit) -> Digit >= $0 andalso Digit =< $9 end, Text)),
    try list_to_integer(Text) catch _:_ -> throw(invalid) end.

validate([Name, DurationText, RateText]) ->
    {Actors, Bytes, Capacity, Hot} = case Name of
        "sparse-256" -> {10000, 256, 1, false};
        "mixed-4k" -> {1000, 4096, 1, false};
        "dense-64k" -> {100, 65536, 1, false};
        "hot-queue" -> {100, 4096, 100, true};
        _ -> throw(invalid)
    end,
    Duration = number(DurationText), Rate = number(RateText),
    Count = (Duration * Rate + 999) div 1000,
    need(Duration >= 1 andalso Duration =< 1000 andalso Rate >= 1 andalso Rate =< 1280000 andalso Count =< 10000),
    #{actors => Actors, bytes => Bytes, capacity => Capacity, hot => Hot, rate => Rate, count => Count};
validate(_) -> throw(invalid).

load_gate() ->
    Path = filename:join(filename:dirname(escript:script_name()), "beam_bounded_mailbox.erl"),
    {ok, beam_bounded_mailbox, Binary, []} = compile:file(Path, [binary, warnings_as_errors, return_errors, return_warnings]),
    {module, beam_bounded_mailbox} = code:load_binary(beam_bounded_mailbox, Path, Binary).

options() -> [monitor, {max_heap_size, #{size => 262144, kill => true, error_logger => false, include_shared_binaries => true}}].

remaining(Deadline) ->
    Left = Deadline - erlang:monotonic_time(millisecond),
    case Left > 0 of true -> Left; false -> error(stage_deadline) end.

run(#{actors := Count, bytes := Bytes, capacity := Capacity, rate := Rate, count := Samples} = Config) ->
    Deadline = erlang:monotonic_time(millisecond) + 3000,
    Parent = self(),
    Entries = [begin
        _ = remaining(Deadline),
        {Pid, Monitor} = spawn_opt(fun() -> initialize(Parent, Identity, Bytes) end, options()),
        Gate = beam_bounded_mailbox:new(Pid, Capacity),
        Pid ! {initialize, Gate},
        {Identity, Pid, Monitor, Gate}
    end || Identity <- lists:seq(1, Count)],
    lists:foreach(fun({Identity, Pid, _Monitor, _Gate}) ->
        receive {ready, Pid, Identity} -> ok after remaining(Deadline) -> error(stage_deadline) end
    end, Entries),
    Routes = maps:from_list([{Identity, {Pid, Gate}} || {Identity, Pid, _, Gate} <- Entries]),
    Initial = #{config => Config, routes => Routes, next => 0, pending => #{},
        samples => array:new(Samples, {fixed, true}), values => #{}, received => #{},
        start => erlang:monotonic_time(nanosecond), deadline => Deadline},
    Result = loop(Initial),
    Values = maps:get(values, Result),
    lists:foreach(fun({_Identity, Pid, _Monitor, Gate}) ->
        0 = beam_bounded_mailbox:queued(Gate),
        ok = beam_bounded_mailbox:close(Gate),
        Pid ! {stop, Parent}
    end, Entries),
    lists:foreach(fun({Identity, Pid, Monitor, _Gate}) ->
        Expected = maps:get(Identity, Values, 0),
        receive {'DOWN', Monitor, process, Pid, {completed, Expected}} -> ok
        after remaining(Deadline) -> error(stage_deadline) end
    end, Entries),
    receive _ -> error(unexpected_trailing_message) after 0 -> ok end,
    #{version => 5, status => <<"ok">>, mode => <<"smoke">>, actors => Count, state_bytes => Bytes,
      mailbox_capacity => Capacity, rate => Rate, deadline_ns => 100000000, pending_capacity => 256,
      message_bytes => 32, states_verified => true,
      samples => [tuple_to_list(Sample) || Sample <- array:to_list(maps:get(samples, Result))],
      committed => [maps:get(Identity, Values, 0) || Identity <- lists:seq(1, Count)],
      environment => #{runtime => <<"beam">>, otp => <<"29">>,
        erts => list_to_binary(erlang:system_info(version)),
        architecture => list_to_binary(erlang:system_info(system_architecture)),
        emu_flavor => erlang:system_info(emu_flavor), schedulers => erlang:system_info(schedulers_online)}}.

initialize(Parent, Identity, Bytes) ->
    State = <<0:64/signed-big, Identity:64, 0:((Bytes - 16) * 8)>>,
    receive {initialize, Gate} ->
        Parent ! {ready, self(), Identity}, counter(Parent, Identity, Gate, State)
    end.

counter(Parent, Identity, Gate, <<Committed:64/signed-big, Identity:64, Padding/binary>> = State) ->
    receive
        {request, Parent, <<Sequence:64, 1:64, 1:64/signed-big, 0:64>>} ->
            ok = beam_bounded_mailbox:started(Gate),
            Candidate = Committed + 1,
            true = Candidate =< 9223372036854775807,
            CandidateState = <<Candidate:64/signed-big, Identity:64, Padding/binary>>,
            Parent ! {reply, self(), <<Sequence:64, 3:64, Candidate:64/signed-big, 0:64>>},
            counter(Parent, Identity, Gate, CandidateState);
        {stop, Parent} ->
            true = byte_size(State) >= 256,
            true = Padding =:= binary:copy(<<0>>, byte_size(Padding)),
            exit({completed, Committed});
        _ -> error(protocol)
    end.

target(Index, false) -> Index rem 100 + 1;
target(Index, true) when Index rem 100 < 90 -> 1;
target(Index, true) -> 2 + (Index div 100 * 10 + Index rem 100 - 90) rem 99.

elapsed(State) -> erlang:monotonic_time(nanosecond) - maps:get(start, State).

loop(State) ->
    _ = remaining(maps:get(deadline, State)),
    Current = collect(State, 32),
    #{config := #{count := Count, rate := Rate}, next := Index, pending := Pending} = Current,
    case Index =:= Count of
        true when map_size(Pending) =:= 0 -> Current;
        true -> erlang:yield(), loop(Current);
        false ->
            Planned = Index * 1000000000 div Rate,
            case elapsed(Current) >= Planned of
                false -> erlang:yield(), loop(Current);
                true -> loop(arrive(Current, Planned))
            end
    end.

arrive(#{config := #{hot := Hot}, next := Index, pending := Pending,
         values := Values, routes := Routes, samples := Samples} = State, Planned) ->
    Identity = target(Index, Hot),
    Sequence = Index + 1,
    Sent = elapsed(State),
    {Pid, Gate} = maps:get(Identity, Routes),
    Admission = case map_size(Pending) =:= 256 of
        true -> {error, pending_full};
        false -> beam_bounded_mailbox:send(Gate, {request, self(), <<Sequence:64, 1:64, 1:64/signed-big, 0:64>>})
    end,
    {Status, NextPending, NextValues} = case Admission of
        ok ->
            Value = maps:get(Identity, Values, 0) + 1,
            {<<"pending">>, Pending#{Sequence => {Identity, Pid, Value}}, Values#{Identity => Value}};
        {error, mailbox_full} -> {<<"mailbox_full">>, Pending, Values};
        {error, pending_full} -> {<<"pending_full">>, Pending, Values};
        _ -> error(admission)
    end,
    Sample = {Sequence, Identity, Planned, Sent, Status, null, null},
    State#{next := Index + 1, pending := NextPending, values := NextValues,
        samples := array:set(Index, Sample, Samples)}.

collect(State, 0) -> State;
collect(#{pending := Pending, samples := Samples, received := Received} = State, Limit) ->
    receive
        {reply, Pid, <<Sequence:64, 3:64, Value:64/signed-big, 0:64>>} ->
            Observed = elapsed(State),
            {Identity, Pid, Value} = maps:get(Sequence, Pending),
            Previous = maps:get(Identity, Received, 0),
            true = Value =:= Previous + 1,
            {Sequence, Identity, Planned, Sent, <<"pending">>, null, null} = array:get(Sequence - 1, Samples),
            Status = case Observed < Planned + 100000000 of true -> <<"reply">>; false -> <<"timed_out">> end,
            Sample = {Sequence, Identity, Planned, Sent, Status, Observed, Value},
            collect(State#{pending := maps:remove(Sequence, Pending),
                received := Received#{Identity => Value}, samples := array:set(Sequence - 1, Sample, Samples)}, Limit - 1);
        _ -> error(unexpected_message)
    after 0 -> State
    end.

emit(Value) -> io:put_chars([json:encode(Value), $\n]).
