#!/usr/bin/env escript
%%! +S 1:1 +A 1 +SDcpu 1:1 +SDio 1 +P 1024 +Q 1024
-mode(compile).

main(Arguments) ->
    try
        true = erlang:system_info(otp_release) =:= "29",
        [LengthText, ConnectionsText, FramesText] = Arguments,
        Length = list_to_integer(LengthText),
        Connections = list_to_integer(ConnectionsText), Frames = list_to_integer(FramesText),
        true = lists:member(Length, [256, 4096, 65536]),
        true = Connections >= 1 andalso Connections =< 8,
        true = Frames >= 1 andalso Frames =< 64,
        Path = filename:join(filename:dirname(escript:script_name()), "beam_bounded_mailbox.erl"),
        {ok, beam_bounded_mailbox, Binary, []} = compile:file(Path, [binary, warnings_as_errors, return_errors, return_warnings]),
        {module, beam_bounded_mailbox} = code:load_binary(beam_bounded_mailbox, Path, Binary),
        run(Length, Connections, Frames), halt(0)
    catch Class:Reason ->
        io:format(standard_error, "beam_tcp_counter: ~p:~p~n", [Class, Reason]), halt(3)
    end.

emit(Value) -> io:put_chars([json:encode(Value), $\n]).

run(Length, Connections, Frames) ->
    Parent = self(),
    {Actor, Monitor} = spawn_opt(fun() ->
        receive {initialize, Gate} ->
            State = <<0:64/signed-big, 1:64, 0:((Length - 16) * 8)>>,
            Parent ! {ready, self()}, counter(Gate, State)
        after 3000 -> error(initialize_deadline) end
    end, [monitor, {max_heap_size, #{size => 262144, kill => true,
                                    error_logger => false, include_shared_binaries => true}}]),
    Gate = beam_bounded_mailbox:new(Actor, 1),
    Actor ! {initialize, Gate},
    receive {ready, Actor} -> ok after 3000 -> error(actor_deadline) end,
    Options = [binary, {packet, raw}, {active, false}, {ip, {127, 0, 0, 1}},
               {backlog, 1}, {sndbuf, 16384}, {recbuf, 16384}, {buffer, 1024},
               {send_timeout, 100}, {send_timeout_close, true}],
    {ok, Listener} = gen_tcp:listen(0, Options),
    {ok, {{127, 0, 0, 1}, Port}} = inet:sockname(Listener),
    Report = #{connections => 0, published => 0, replies => 0, rejected => 0,
               timed_out => 0, failed => 0, invalid => 0, io_errors => 0,
               max_socket_buffer => socket_buffers(Listener)},
    Route = #{pid => Actor, monitor => Monitor, gate => Gate, dead => false, committed => 0},
    emit(#{listening => list_to_binary("127.0.0.1:" ++ integer_to_list(Port))}),
    {Final, Result} = connections(Listener, Connections, Frames, Route, Report),
    ok = gen_tcp:close(Listener),
    {Committed, FullState} = finish(Final, Length),
    emit(Result#{committed => Committed, state_bytes => Length, full_state_verified => FullState,
                 resources_released => true, gate_closed => beam_bounded_mailbox:queued(Gate) =:= closed,
                 actor_stopped => not is_process_alive(Actor), otp_release => <<"29">>}).

socket_buffers(Socket) ->
    {ok, Sizes} = inet:getopts(Socket, [sndbuf, recbuf]),
    Values = [Value || {_, Value} <- Sizes],
    case lists:all(fun(Value) -> Value > 0 andalso Value =< 65536 end, Values) of
        true -> ok;
        false -> error({socket_buffer_exceeds_limit, Sizes})
    end,
    lists:max(Values).

connections(_Listener, 0, _Frames, Route, Report) -> {Route, Report};
connections(Listener, Remaining, Frames, Route, Report) ->
    case gen_tcp:accept(Listener, 2000) of
        {ok, Socket} ->
            ok = inet:setopts(Socket, [{nodelay, true}]),
            case os:type() of
                {unix, darwin} -> ok = inet:setopts(Socket, [{raw, 6, 2, <<1024:32/native>>}]);
                _ -> ok
            end,
            ok = inet:setopts(Socket, [{sndbuf, 16384}, {recbuf, 16384}]),
            Size = max(maps:get(max_socket_buffer, Report), socket_buffers(Socket)),
            {NextRoute, NextReport} = frames(Socket, Frames, Route,
                Report#{connections := maps:get(connections, Report) + 1, max_socket_buffer := Size}),
            ok = gen_tcp:close(Socket),
            connections(Listener, Remaining - 1, Frames, NextRoute, NextReport);
        {error, timeout} -> {Route, Report};
        {error, Reason} -> error({accept_failed, Reason})
    end.

frames(_Socket, 0, Route, Report) -> {Route, Report};
frames(Socket, Remaining, Route, Report) ->
    Received = gen_tcp:recv(Socket, 32, 100),
    Observed = Report#{max_socket_buffer := max(maps:get(max_socket_buffer, Report), socket_buffers(Socket))},
    case Received of
        {ok, <<Sequence:64, Operation:64, Delta:64/signed-big, 0:64>>}
                when Sequence > 0, Operation >= 1, Operation =< 2 ->
            {ReplyOperation, Value, NextRoute, Key} = request(Route, Delta, Operation =:= 2),
            Next = increment(Key, Observed),
            Published = case Key of rejected -> Next; _ -> increment(published, Next) end,
            case gen_tcp:send(Socket, <<Sequence:64, ReplyOperation:64, Value:64/signed-big, 0:64>>) of
                ok ->
                    Size = max(maps:get(max_socket_buffer, Published), socket_buffers(Socket)),
                    frames(Socket, Remaining - 1, NextRoute, Published#{max_socket_buffer := Size});
                {error, _} -> {NextRoute, increment(io_errors, Published)}
            end;
        {ok, _} -> {Route, increment(invalid, Observed)};
        {error, closed} -> {Route, Observed};
        {error, _} -> {Route, increment(io_errors, Observed)}
    end.

increment(Key, Report) -> Report#{Key := maps:get(Key, Report) + 1}.

request(#{dead := true} = Route, _Delta, _Fail) -> {4, 4, Route, rejected};
request(#{pid := Actor, monitor := Monitor, gate := Gate} = Route, Delta, Fail) ->
    Reference = make_ref(),
    case beam_bounded_mailbox:send(Gate, {request, self(), Reference, Delta, Fail}) of
        ok ->
            receive
                {reply, Actor, Reference, Value} -> {3, Value, Route#{committed := Value}, replies};
                {'DOWN', Monitor, process, Actor, {handler_failed, Committed, Length, Prefix}} ->
                    {4, 3, Route#{dead := true, committed := Committed,
                                 failed_length => Length, failed_prefix => Prefix}, failed};
                {'DOWN', Monitor, process, Actor, Reason} -> error({unexpected_actor_exit, Reason})
            after 50 -> error(request_timeout_not_equivalent) end;
        {error, _} -> {4, 4, Route, rejected}
    end.

counter(Gate, <<Committed:64/signed-big, Tail/binary>> = State) ->
    receive
        {request, From, Reference, Delta, Fail} ->
            ok = beam_bounded_mailbox:started(Gate),
            Candidate = Committed + Delta,
            case Candidate < -9223372036854775808 orelse Candidate > 9223372036854775807 of
                true -> fail(Gate, Committed, State);
                false -> ok
            end,
            CandidateState = <<Candidate:64/signed-big, Tail/binary>>,
            case Fail of
                true -> fail(Gate, Committed, State);
                false -> From ! {reply, self(), Reference, Candidate}, counter(Gate, CandidateState)
            end;
        {inspect, From, Reference} ->
            From ! {state, self(), Reference, State}, counter(Gate, State);
        stop -> beam_bounded_mailbox:close(Gate), exit(normal)
    end.

fail(Gate, Committed, State) ->
    ok = beam_bounded_mailbox:close(Gate),
    exit({handler_failed, Committed, byte_size(State), binary:part(State, 0, 32)}).

finish(#{dead := true, committed := Value, failed_length := Length, failed_prefix := Prefix}, Length) ->
    <<Value:64/signed-big, 1:64, 0:128>> = Prefix,
    {Value, false};
finish(#{pid := Actor, monitor := Monitor, committed := Value}, Length) ->
    Reference = make_ref(), Actor ! {inspect, self(), Reference},
    receive
        {state, Actor, Reference, State} ->
            Length = byte_size(State),
            <<Value:64/signed-big, 1:64, Zero/binary>> = State,
            true = Zero =:= binary:copy(<<0>>, Length - 16)
    after 3000 -> error(final_state_deadline) end,
    Actor ! stop,
    receive {'DOWN', Monitor, process, Actor, normal} -> ok
    after 3000 -> error(stop_deadline) end,
    {Value, true}.
