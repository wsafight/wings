#!/usr/bin/env escript
%%! +S 2:2 +A 1 +SDcpu 1:1 +SDio 1 +P 1024 +Q 1024
-mode(compile).

main(Arguments) ->
    try
        need(Arguments =:= []),
        need(erlang:system_info(otp_release) =:= "29"),
        io:setopts(standard_io, [binary]),
        Input = case file:read(standard_io, 65537) of
            {ok, Data} -> Data; eof -> throw(invalid); _ -> error(input_read)
        end,
        need(byte_size(Input) =< 65536),
        Path = filename:join(filename:dirname(escript:script_name()), "beam_failure_counter.erl"),
        {ok, beam_failure_counter, Binary, []} = compile:file(Path, [binary, warnings_as_errors, return_errors, return_warnings]),
        {module, beam_failure_counter} = code:load_binary(beam_failure_counter, Path, Binary),
        Config = beam_failure_counter:decode(Input),
        beam_failure_counter:validate(Config),
        beam_failure_counter:load_gate(),
        Result = beam_failure_counter:run(Config),
        emit(Result),
        case maps:get(status, Result) of <<"ok">> -> halt(0); _ -> halt(1) end
    catch
        throw:invalid -> emit(#{version => 9, status => <<"invalid">>}), halt(2);
        _:_ -> emit(#{version => 9, status => <<"oracle_error">>}), halt(3)
    end.

need(true) -> ok;
need(false) -> throw(invalid).

emit(Value) -> io:put_chars([json:encode(Value), $\n]).
