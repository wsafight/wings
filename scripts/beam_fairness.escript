#!/usr/bin/env escript
%%! +S 1:1 +A 1 +SDcpu 1:1 +SDio 1 +P 1024 +Q 1024
-mode(compile).

main(Arguments) ->
    try
        {Count, WarmupSec, SampleSec, Destination} = case Arguments of
            ["10"] -> {100, 0, 0, none};
            ["30"] -> {300, 0, 0, none};
            ["--measure", Path] when Path =/= [] ->
                {W, S} = bench_window(),
                {10000 * (W + S), W, S, Path};
            _ -> throw(invalid)
        end,
        Otp = erlang:system_info(otp_release),
        need(lists:member(Otp, ["27", "28", "29"])),
        load_gate(),
        emit(run(Count, WarmupSec, SampleSec, Destination)), halt(0)
    catch
        throw:invalid -> emit(#{version => version(Arguments), status => <<"invalid">>}), halt(2);
        _:_ -> emit(#{version => version(Arguments), status => <<"oracle_error">>}), halt(3)
    end.

version(["--measure" | _]) -> 8;
version(_) -> 7.

need(true) -> ok;
need(false) -> throw(invalid).

number(Text) ->
    need(Text =/= [] andalso lists:all(fun(Digit) -> Digit >= $0 andalso Digit =< $9 end, Text)),
    try list_to_integer(Text) catch _:_ -> throw(invalid) end.

bench_window() ->
    case os:getenv("WINGS_BENCH_WINDOW") of
        false -> {30, 60};
        "" -> {30, 60};
        Raw ->
            case string:split(Raw, ",", all) of
                [WText, SText] ->
                    W = number(WText), S = number(SText),
                    need(W >= 1 andalso W =< 60 andalso S >= 1 andalso S =< 120),
                    {W, S};
                _ -> throw(invalid)
            end
    end.

load_gate() ->
    Path = filename:join(filename:dirname(escript:script_name()), "beam_bounded_mailbox.erl"),
    {ok, beam_bounded_mailbox, Binary, []} = compile:file(Path, [binary, warnings_as_errors, return_errors, return_warnings]),
    {module, beam_bounded_mailbox} = code:load_binary(beam_bounded_mailbox, Path, Binary).

remaining(Deadline) ->
    Left = Deadline - erlang:monotonic_time(millisecond),
    case Left > 0 of true -> Left; false -> error(stage_deadline) end.

run(Count, WarmupSec, SampleSec, Destination) ->
    Measure = Destination =/= none,
    Deadline = erlang:monotonic_time(millisecond) + case Measure of true -> (WarmupSec + SampleSec + 30) * 1000; false -> 3000 end,
    Recorder = case Measure of
        true -> {ok, File} = file:open(Destination, [write, binary, raw, exclusive]), {trace, File, [], 0};
        false -> {samples, array:new(Count, {fixed, true})}
    end,
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
    Initial = #{next => 0, count => Count, warmup_sec => WarmupSec, sample_sec => SampleSec,
        pending => #{}, hot_pending => none, hot => [], hot_count => 0,
        values => #{}, received => #{}, routes => Routes, recorder => Recorder, sampling => false,
        start => erlang:monotonic_time(nanosecond), deadline => Deadline},
    case Measure of true -> emit(#{event => <<"start">>, warmup_seconds => WarmupSec, sample_seconds => SampleSec}); false -> ok end,
    Result = loop(Initial),
    MeasurementElapsed = elapsed(Result),
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
    #{version => case Measure of true -> 8; false -> 7 end, status => <<"ok">>,
      mode => case Measure of true -> <<"measure">>; false -> <<"smoke">> end, actors => 101, state_bytes => 256,
      mailbox_capacity => 1, pending_capacity => 255, deadline_ns => 100000000, message_bytes => 32,
      samples => finish(maps:get(recorder, Result)),
      hot => lists:reverse(maps:get(hot, Result)), states_verified => true,
      measurement_elapsed_ns => MeasurementElapsed,
      committed => [maps:get(Identity, Values, 0) || Identity <- lists:seq(1, 101)],
      environment => #{runtime => <<"beam">>, otp => list_to_binary(erlang:system_info(otp_release)), schedulers => erlang:system_info(schedulers_online)}}.

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
    Current = sampling(start_hot(collect(State, 32))),
    #{next := Next, count := Count, warmup_sec := WarmupSec, sample_sec := SampleSec, pending := Pending, hot_pending := Hot} = Current,
    TotalNs = (WarmupSec + SampleSec) * 1000000000,
    Active = case maps:get(recorder, Current) of {trace, _, _, _} -> elapsed(Current) < TotalNs; _ -> false end,
    case Next =:= Count andalso map_size(Pending) =:= 0 andalso Hot =:= none andalso not Active of
        true -> Current;
        false ->
            case Next < Count andalso elapsed(Current) >= planned(Next) of
                true -> loop(arrive(Current));
                false -> erlang:yield(), loop(Current)
            end
    end.

start_hot(#{next := Next, count := Count, warmup_sec := WarmupSec, sample_sec := SampleSec, hot_pending := none, hot_count := Completed, routes := Routes, recorder := Recorder} = State) ->
    TotalNs = (WarmupSec + SampleSec) * 1000000000,
    Active = case Recorder of {trace, _, _, _} -> elapsed(State) < TotalNs; _ -> false end,
    case Next < Count orelse Active of
        true ->
            true = Completed < case Recorder of {trace, _, _, _} -> 1000000; _ -> 10000 end,
            {_Pid, Gate} = maps:get(1, Routes),
            Sent = elapsed(State), Sequence = Completed + 1,
            ok = beam_bounded_mailbox:send(Gate, {request, self(), <<Sequence:64, 2:64, 0:128>>}),
            State#{hot_pending := {Sequence, Sent}, recorder := record_hot(Recorder, Sequence, Sent, 0, 0, 5)};
        false -> State
    end;
start_hot(State) -> State.

arrive(#{next := Index, routes := Routes, recorder := Recorder, pending := Pending, values := Values} = State) ->
    Identity = 2 + Index rem 100, Sequence = Index + 1, Planned = planned(Index), Sent = elapsed(State),
    {Pid, Gate} = maps:get(Identity, Routes),
    Admission = case map_size(Pending) =:= 255 of
        true -> {error, pending_full};
        false -> beam_bounded_mailbox:send(Gate, {request, self(), <<Sequence:64, 1:64, 1:64/signed-big, 0:64>>})
    end,
    {Status, NextPending, NextValues} = case Admission of
        ok -> Value = maps:get(Identity, Values, 0) + 1,
            {<<"pending">>, Pending#{Sequence => {Identity, Pid, Value, Sent}}, Values#{Identity => Value}};
        {error, mailbox_full} -> {<<"mailbox_full">>, Pending, Values};
        {error, pending_full} -> {<<"pending_full">>, Pending, Values}
    end,
    Sample = {Sequence, Identity, Planned, Sent, Status, null, null},
    State#{next := Index + 1, pending := NextPending, values := NextValues, recorder := record_sample(Recorder, Index, Sample)}.

collect(State, 0) -> State;
collect(#{routes := Routes, hot_pending := Hot, hot := History, hot_count := Completed, values := Values,
          pending := Pending, received := Received, recorder := Recorder} = State, Limit) ->
    {HotPid, _Gate} = maps:get(1, Routes),
    receive
        {reply, HotPid, <<Sequence:64, 3:64, 1000000:64/signed-big, 0:64>>} ->
            Observed = elapsed(State),
            {Sequence, Sent} = Hot,
            true = Sequence =:= Completed + 1,
            NextHistory = case Recorder of {trace, _, _, _} -> History; _ -> [[Sequence, Sent, Observed, 1000000] | History] end,
            collect(State#{hot_pending := none, hot_count := Sequence, hot := NextHistory,
                recorder := record_hot(Recorder, Sequence, Sent, Observed, 1000000, 6), values := Values#{1 => Sequence}}, Limit - 1);
        {reply, Pid, <<Sequence:64, 3:64, Value:64/signed-big, 0:64>>} ->
            Observed = elapsed(State),
            {Identity, Pid, Value, Sent} = maps:get(Sequence, Pending),
            true = Value =:= maps:get(Identity, Received, 0) + 1,
            Planned = planned(Sequence - 1),
            Status = case Observed < Planned + 100000000 of true -> <<"reply">>; false -> <<"timed_out">> end,
            Sample = {Sequence, Identity, Planned, Sent, Status, Observed, Value},
            collect(State#{pending := maps:remove(Sequence, Pending), received := Received#{Identity => Value},
                recorder := record_sample(Recorder, Sequence - 1, Sample)}, Limit - 1);
        _ -> error(unexpected_message)
    after 0 -> State
    end.

emit(Value) -> io:put_chars([json:encode(Value), $\n]).

sampling(#{recorder := {trace, _, _, _}, warmup_sec := WarmupSec, sampling := false} = State) ->
    case elapsed(State) >= WarmupSec * 1000000000 of
        true -> emit(#{event => <<"sampling">>, elapsed_ns => elapsed(State)}), State#{sampling := true};
        false -> State
    end;
sampling(State) -> State.

record_sample({samples, Samples}, Index, Sample) -> {samples, array:set(Index, Sample, Samples)};
record_sample(Recorder, _Index, {Sequence, _Identity, _Planned, Sent, Status, Observed, Value}) ->
    {Kind, Completion, Reply} = case Status of
        <<"pending">> -> {0, 0, 0}; <<"reply">> -> {1, Observed, Value};
        <<"timed_out">> -> {2, Observed, Value}; <<"mailbox_full">> -> {3, 0, 0}; <<"pending_full">> -> {4, 0, 0}
    end,
    record(Recorder, <<Sequence:64, Sent:64, Completion:64, Reply:64/signed-big, Kind:64>>).

record_hot({samples, _} = Recorder, _Sequence, _Sent, _Observed, _Value, _Kind) -> Recorder;
record_hot(Recorder, Sequence, Sent, Observed, Value, Kind) ->
    record(Recorder, <<Sequence:64, Sent:64, Observed:64, Value:64/signed-big, Kind:64>>).

record({trace, File, Buffer, Size}, Event) when Size + 40 > 65536 ->
    ok = file:write(File, lists:reverse(Buffer)), {trace, File, [Event], 40};
record({trace, File, Buffer, Size}, Event) -> {trace, File, [Event | Buffer], Size + 40}.

finish({samples, Samples}) -> [tuple_to_list(Sample) || Sample <- array:to_list(Samples)];
finish({trace, File, Buffer, _Size}) ->
    ok = file:write(File, lists:reverse(Buffer)), ok = file:close(File), [].
