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
        throw:invalid -> emit(#{version => version(Arguments), status => <<"invalid">>}), halt(2);
        _:_ -> emit(#{version => version(Arguments), status => <<"oracle_error">>}), halt(3)
    end.

version(["--measure" | _]) -> 6;
version(_) -> 5.

need(true) -> ok;
need(false) -> throw(invalid).

number(Text) ->
    need(Text =/= [] andalso lists:all(fun(Digit) -> Digit >= $0 andalso Digit =< $9 end, Text)),
    try list_to_integer(Text) catch _:_ -> throw(invalid) end.

validate(["--measure", Name, RateText, Path]) ->
    Config = validate([Name, "1", RateText]),
    #{rate := Rate, hot := Hot} = Config,
    Base = case Hot of true -> 10000; false -> 1000 end,
    need(lists:member(Rate, [Base bsl Power || Power <- lists:seq(0, 7)])),
    Config#{count := Rate * 90, trace => Path};
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
    Measure = maps:is_key(trace, Config),
    Recorder = case Measure of
        true ->
            {ok, File} = file:open(maps:get(trace, Config), [write, binary, raw, exclusive]),
            {trace, File, [], 0};
        false -> {samples, array:new(Samples, {fixed, true})}
    end,
    Deadline = erlang:monotonic_time(millisecond) + case Measure of true -> 120000; false -> 3000 end,
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
    case Measure of true -> emit(#{event => <<"start">>, warmup_seconds => 30, sample_seconds => 60}); false -> ok end,
    Initial = #{config => Config, routes => Routes, next => 0, pending => #{},
        recorder => Recorder, sampling => false, values => #{}, received => #{},
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
    OutputSamples = finish(maps:get(recorder, Result)),
    #{version => case Measure of true -> 6; false -> 5 end, status => <<"ok">>,
      mode => case Measure of true -> <<"measure">>; false -> <<"smoke">> end, actors => Count, state_bytes => Bytes,
      mailbox_capacity => Capacity, rate => Rate, deadline_ns => 100000000, pending_capacity => 256,
      message_bytes => 32, states_verified => true,
      samples => OutputSamples,
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
    Collected = collect(State, 32),
    Current = sampling(Collected),
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
         values := Values, routes := Routes, recorder := Recorder} = State, Planned) ->
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
            {<<"pending">>, Pending#{Sequence => {Identity, Pid, Value, Sent}}, Values#{Identity => Value}};
        {error, mailbox_full} -> {<<"mailbox_full">>, Pending, Values};
        {error, pending_full} -> {<<"pending_full">>, Pending, Values};
        _ -> error(admission)
    end,
    Sample = {Sequence, Identity, Planned, Sent, Status, null, null},
    State#{next := Index + 1, pending := NextPending, values := NextValues,
        recorder := arrival(Recorder, Index, Sample)}.

collect(State, 0) -> State;
collect(#{pending := Pending, recorder := Recorder, received := Received, config := #{rate := Rate}} = State, Limit) ->
    receive
        {reply, Pid, <<Sequence:64, 3:64, Value:64/signed-big, 0:64>>} ->
            Observed = elapsed(State),
            {Identity, Pid, Value, Sent} = maps:get(Sequence, Pending),
            Previous = maps:get(Identity, Received, 0),
            true = Value =:= Previous + 1,
            Planned = (Sequence - 1) * 1000000000 div Rate,
            Status = case Observed < Planned + 100000000 of true -> <<"reply">>; false -> <<"timed_out">> end,
            Sample = {Sequence, Identity, Planned, Sent, Status, Observed, Value},
            collect(State#{pending := maps:remove(Sequence, Pending),
                received := Received#{Identity => Value}, recorder := reply(Recorder, Sequence - 1, Sample)}, Limit - 1);
        _ -> error(unexpected_message)
    after 0 -> State
    end.

sampling(#{config := Config, sampling := false} = State) ->
    case maps:is_key(trace, Config) andalso elapsed(State) >= 30000000000 of
        true -> emit(#{event => <<"sampling">>, elapsed_ns => elapsed(State)}), State#{sampling := true};
        false -> State
    end;
sampling(State) -> State.

arrival({samples, Samples}, Index, Sample) -> {samples, array:set(Index, Sample, Samples)};
arrival(Recorder, _Index, {Sequence, _Identity, _Planned, Sent, Status, null, null}) ->
    Kind = case Status of <<"pending">> -> 0; <<"mailbox_full">> -> 3; <<"pending_full">> -> 4 end,
    record(Recorder, <<Sequence:64, Sent:64, 0:128, Kind:64>>).

reply({samples, Samples}, Index, Sample) -> {samples, array:set(Index, Sample, Samples)};
reply(Recorder, _Index, {Sequence, _Identity, _Planned, Sent, Status, Observed, Value}) ->
    Kind = case Status of <<"reply">> -> 1; <<"timed_out">> -> 2 end,
    record(Recorder, <<Sequence:64, Sent:64, Observed:64, Value:64/signed-big, Kind:64>>).

record({trace, File, Buffer, Size}, Event) when Size + 40 > 65536 ->
    ok = file:write(File, lists:reverse(Buffer)), {trace, File, [Event], 40};
record({trace, File, Buffer, Size}, Event) -> {trace, File, [Event | Buffer], Size + 40}.

finish({samples, Samples}) -> [tuple_to_list(Sample) || Sample <- array:to_list(Samples)];
finish({trace, File, Buffer, _Size}) ->
    ok = file:write(File, lists:reverse(Buffer)), ok = file:close(File), [].

emit(Value) -> io:put_chars([json:encode(Value), $\n]).
