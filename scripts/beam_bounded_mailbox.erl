-module(beam_bounded_mailbox).
-export([new/2, send/2, started/1, close/1, queued/1]).

new(Actor, Capacity) when is_pid(Actor), is_integer(Capacity), Capacity >= 1, Capacity =< 256 ->
    {bounded_mailbox, Actor, Capacity, atomics:new(1, [{signed, true}])};
new(_, _) -> error(badarg).

send({bounded_mailbox, Actor, Capacity, State}, Message) ->
    case reserve(State, Capacity, atomics:get(State, 1)) of
        ok -> Actor ! Message, ok;
        Error -> Error
    end.

reserve(_State, _Capacity, -1) -> {error, closed};
reserve(_State, Capacity, Capacity) -> {error, mailbox_full};
reserve(State, Capacity, Current) ->
    case atomics:compare_exchange(State, 1, Current, Current + 1) of
        ok -> ok;
        Actual -> reserve(State, Capacity, Actual)
    end.

started({bounded_mailbox, Actor, _Capacity, State}) when Actor =:= self() ->
    release(State, atomics:get(State, 1));
started(_) -> {error, foreign_actor}.

release(_State, -1) -> {error, closed};
release(_State, 0) -> {error, not_queued};
release(State, Current) ->
    case atomics:compare_exchange(State, 1, Current, Current - 1) of
        ok -> ok;
        Actual -> release(State, Actual)
    end.

close({bounded_mailbox, _Actor, _Capacity, State}) ->
    atomics:exchange(State, 1, -1), ok.

queued({bounded_mailbox, _Actor, _Capacity, State}) ->
    case atomics:get(State, 1) of -1 -> closed; Count -> Count end.
