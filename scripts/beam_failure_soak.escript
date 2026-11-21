#!/usr/bin/env escript
%%! +S 2:2 +A 1 +SDcpu 1:1 +SDio 1 +P 1024 +Q 1024
-mode(compile).

main(Arguments) ->
    try
        {Soak, Trace} = case Arguments of
            ["--soak", Path] -> {true, Path};
            ["--verify-soak", Path] -> {false, Path};
            _ -> throw(invalid)
        end,
        need(erlang:system_info(otp_release) =:= "29"),
        io:setopts(standard_io, [binary]),
        Input = case file:read(standard_io, 65537) of
            {ok, Data} -> Data; _ -> throw(invalid)
        end,
        need(byte_size(Input) =< 65536),
        Source = filename:join(filename:dirname(escript:script_name()), "beam_failure_counter.erl"),
        {ok, beam_failure_counter, Binary, []} = compile:file(Source, [binary, warnings_as_errors, return_errors, return_warnings]),
        {module, beam_failure_counter} = code:load_binary(beam_failure_counter, Source, Binary),
        Config = beam_failure_counter:decode(Input),
        beam_failure_counter:validate(Config),
        validate(Config),
        beam_failure_counter:load_gate(),
        run(Config, Soak, Trace), halt(0)
    catch
        throw:invalid -> emit(#{version => 10, status => <<"invalid">>}), halt(2);
        Class:Reason ->
            io:format(standard_error, "~P:~P~n", [Class, 3, Reason, 8]),
            emit(#{version => 10, status => <<"oracle_error">>}), halt(3)
    end.

need(true) -> ok;
need(false) -> throw(invalid).

validate(Config) ->
    Streams = maps:get(<<"streams">>, Config),
    need(lists:all(fun(Stream) -> Stream =/= [] end, Streams)),
    Requests = lists:append(Streams),
    Failed = maps:from_list([{maps:get(<<"actor">>, Request), true}
                            || Request <- Requests, maps:get(<<"fail">>, Request)]),
    need(map_size(Failed) > 0),
    need(lists:any(fun(Request) -> not maps:is_key(maps:get(<<"actor">>, Request), Failed) end, Requests)).

now(Start) ->
    Elapsed = erlang:monotonic_time(nanosecond) - Start,
    case Elapsed < 120000000000 of true -> Elapsed; false -> error(stage_deadline) end.

wait(Start, Planned) ->
    case Planned - now(Start) of
        Delay when Delay > 0 ->
            receive after min(10, max(1, (Delay + 999999) div 1000000)) -> ok end,
            wait(Start, Planned);
        _ -> ok
    end.

run(Config, Soak, Path) ->
    {ok, File} = file:open(Path, [write, exclusive, binary, raw, {delayed_write, 65536, 1000}]),
    Count = case Soak of true -> 900; false -> 3 end,
    Mode = case Soak of true -> <<"soak">>; false -> <<"verify">> end,
    emit(#{version => 10, event => <<"start">>, mode => Mode, epochs => Count,
        warmup_seconds => case Soak of true -> 30; false -> 0 end,
        sample_seconds => case Soak of true -> 60; false -> 0 end,
        period_ns => case Soak of true -> 100000000; false -> 0 end,
        round_deadline_ns => 3000000000, trace_limit_bytes => 67108864,
        cold_bytes => null, stack_slot_bytes => null, max_cached_stacks => null,
        environment => #{runtime => <<"beam">>, otp => <<"29">>, schedulers => 2,
                         senders => 2, window_per_sender => 16}}),
    Start = erlang:monotonic_time(nanosecond),
    try
        {Bytes, Checks, Sampling} = epochs(1, Count, Soak, Start, Config, File, [], 0, 0, false),
        case Soak of true -> wait(Start, 90000000000), true = Sampling; false -> ok end,
        Elapsed = now(Start),
        ok = file:sync(File), ok = file:close(File),
        now(Start),
        emit(#{version => 10, event => <<"complete">>, status => <<"ok">>, mode => Mode,
               epochs => Count, stale_identities_checked => Checks, measurement_elapsed_ns => Elapsed,
               trace_bytes => Bytes, actors_and_senders_terminated => true})
    after file:close(File) end.

epochs(Sequence, Count, _Soak, _Start, _Config, _File, _Previous, Bytes, Checks, Sampling)
        when Sequence > Count -> {Bytes, Checks, Sampling};
epochs(Sequence, Count, Soak, Start, Config, File, Previous, Bytes, Checks, Sampling) ->
    Planned = case Soak of true -> (Sequence - 1) * 100000000; false -> 0 end,
    wait(Start, Planned),
    Started = now(Start),
    NextSampling = case Soak andalso not Sampling andalso Started >= 30000000000 of
        true -> emit(#{version => 10, event => <<"sampling">>, elapsed_ns => Started}), true;
        false -> Sampling
    end,
    Deadline = (Start + Started + 3000000000) div 1000000,
    {Result, Actors, Deadline} = beam_failure_counter:epoch(Config, Previous, Deadline),
    Stale = length(Actors) + length(Previous),
    Ended = now(Start),
    beam_failure_counter:remaining(Deadline),
    Record = #{version => 10, epoch => Sequence, planned_ns => Planned, started_ns => Started,
        completed_ns => Ended, stale_identities_checked => Stale,
        retained_bytes => null, retained_processes => null, retained_messages => null,
        retained_requests => null, cached_stacks => null, result => Result,
        beam_process_count => erlang:system_info(process_count),
        beam_port_count => erlang:system_info(port_count)},
    Line = [json:encode(Record), $\n],
    NextBytes = Bytes + iolist_size(Line),
    true = NextBytes =< 67108864,
    ok = file:write(File, Line),
    epochs(Sequence + 1, Count, Soak, Start, Config, File, Actors, NextBytes, Checks + Stale, NextSampling).

emit(Value) -> io:put_chars([json:encode(Value), $\n]).
