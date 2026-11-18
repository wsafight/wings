#!/usr/bin/env escript
%%! +A 1 +SDcpu 1:1 +SDio 1 +P 1024 +Q 1024
-mode(compile).

main(Arguments) ->
    try
        {Soak, Workers, Path} = case Arguments of
            ["--verify-reuse", "1", Target] -> {false, 1, Target};
            ["--verify-reuse", "2", Target] -> {false, 2, Target};
            ["--soak", "1", Target] -> {true, 1, Target};
            ["--soak", "2", Target] -> {true, 2, Target};
            _ -> throw(invalid)
        end,
        true = erlang:system_info(otp_release) =:= "29",
        Workers = erlang:system_info(schedulers),
        Workers = erlang:system_info(schedulers_online),
        ok = logger:set_primary_config(level, emergency),
        lists:foreach(fun(Module) ->
            Source = filename:join(filename:dirname(escript:script_name()), atom_to_list(Module) ++ ".erl"),
            {ok, Module, Binary, []} = compile:file(Source, [binary, warnings_as_errors, return_errors, return_warnings]),
            {module, Module} = code:load_binary(Module, Source, Binary)
        end, [beam_bounded_mailbox, beam_recovery_counter]),
        run(Soak, Workers, Path), halt(0)
    catch
        throw:invalid -> halt(2);
        Class:Reason:Stack ->
            io:format(standard_error, "~P:~P ~P~n", [Class, 3, Reason, 8, Stack, 8]), halt(3)
    end.

now(Start) ->
    Elapsed = erlang:monotonic_time(nanosecond) - Start,
    true = Elapsed < 120000000000,
    Elapsed.

wait(Start, Planned) ->
    case Planned - now(Start) of
        Delay when Delay > 0 ->
            receive after min(10, max(1, (Delay + 999999) div 1000000)) -> ok end,
            wait(Start, Planned);
        _ -> ok
    end.

run(Soak, Workers, Path) ->
    {ok, File} = file:open(Path, [write, exclusive, binary, raw, {delayed_write, 65536, 1000}]),
    {ok, Root} = supervisor:start_link(beam_recovery_counter, root),
    {ok, Broker} = gen_server:start_link(beam_recovery_counter, broker, []),
    Count = case Soak of true -> 900; false -> 3 end,
    Mode = case Soak of true -> <<"soak">>; false -> <<"verify">> end,
    emit(#{version => 11, event => <<"start">>, mode => Mode, epochs => Count,
        warmup_seconds => case Soak of true -> 30; false -> 0 end,
        sample_seconds => case Soak of true -> 60; false -> 0 end,
        period_ns => case Soak of true -> 100000000; false -> 0 end,
        round_deadline_ns => 3000000000, trace_limit_bytes => 67108864,
        cold_bytes => null, stack_slot_bytes => null, max_cached_stacks => null,
        environment => #{runtime => <<"beam">>, otp => <<"29">>, schedulers => Workers,
                         senders => 2, mailbox => 16, result_slots => 17, state_bytes => 4096}}),
    Start = erlang:monotonic_time(nanosecond),
    try
        {Bytes, Checks, Sampling} = epochs(1, Count, Soak, Start, Root, Broker, File, {[], []}, 0, 0, false),
        case Soak of true -> wait(Start, 90000000000), true = Sampling; false -> ok end,
        [] = supervisor:which_children(Root),
        ok = gen_server:stop(Broker, normal, 1000),
        ok = gen_server:stop(Root, normal, 1000),
        Elapsed = now(Start),
        ok = file:sync(File), ok = file:close(File),
        now(Start),
        emit(#{version => 11, event => <<"complete">>, status => <<"ok">>, mode => Mode, epochs => Count,
            measurement_elapsed_ns => Elapsed, stale_identities_checked => Checks, trace_bytes => Bytes,
            actors_and_senders_terminated => true, protocol_slots_released => true})
    after file:close(File) end.

epochs(Sequence, Count, _Soak, _Start, _Root, _Broker, _File, _Previous, Bytes, Checks, Sampling)
        when Sequence > Count -> {Bytes, Checks, Sampling};
epochs(Sequence, Count, Soak, Start, Root, Broker, File, Previous, Bytes, Checks, Sampling) ->
    Planned = case Soak of true -> (Sequence - 1) * 100000000; false -> 0 end,
    wait(Start, Planned), Started = now(Start),
    NextSampling = case Soak andalso not Sampling andalso Started >= 30000000000 of
        true -> emit(#{version => 11, event => <<"sampling">>, elapsed_ns => Started}), true;
        false -> Sampling
    end,
    Deadline = (Start + Started + 3000000000) div 1000000,
    {Result, Actors, Stale} = beam_recovery_counter:epoch(Root, Broker, Previous, Deadline),
    Ended = now(Start), beam_recovery_counter:remaining(Deadline),
    Record = #{version => 11, epoch => Sequence, planned_ns => Planned, started_ns => Started,
        completed_ns => Ended, stale_identities_checked => Stale, retained => null, cached_stacks => null,
        result => Result, live_protocol_slots => 0, beam_process_count => erlang:system_info(process_count),
        beam_port_count => erlang:system_info(port_count)},
    Line = [json:encode(Record), $\n],
    NextBytes = Bytes + iolist_size(Line), true = NextBytes =< 67108864,
    ok = file:write(File, Line),
    epochs(Sequence + 1, Count, Soak, Start, Root, Broker, File, Actors, NextBytes, Checks + Stale, NextSampling).

emit(Value) -> io:put_chars([json:encode(Value), $\n]).
