use wings_sema::{Analysis, SymbolKind, Type, analyze};
use wings_source::MAX_DIAGNOSTICS;

fn checked(text: &str) -> Analysis {
    let analysis = analyze(text);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    analysis
}

fn handler(body: &str) -> String {
    format!("process P {{ state count: Int = 0; on Get(value: Int) -> Int {{ {body} }} }}")
}

fn assert_error(text: &str, code: &str, fragment: &str) {
    let analysis = analyze(text);
    let diagnostic = analysis
        .diagnostics
        .iter()
        .find(|d| d.code == code && d.message.contains(fragment));
    assert!(
        diagnostic.is_some(),
        "expected {code} containing {fragment:?}, got {:?}",
        analysis.diagnostics
    );
    for diagnostic in &analysis.diagnostics {
        assert!(
            text.get(diagnostic.span.start..diagnostic.span.end)
                .is_some(),
            "{diagnostic:?}"
        );
    }
}

#[test]
fn self_send_checks_forward_one_way_protocol_and_handler_scope() {
    let text = "process P { state n: Int = 0; on Start { send_self(Add, 1, true, ()) } on Add(v: Int, b: Bool, u: Unit) { n = v } }";
    let analysis = checked(text);
    let target = text.find("Add, 1").unwrap();
    let reference = analysis
        .references
        .iter()
        .find(|reference| reference.span.start == target)
        .unwrap();
    assert_eq!(analysis.symbols[reference.symbol].kind, SymbolKind::Handler);
    for (body, code, fragment) in [
        ("send_self()", "T013", "static message"),
        ("send_self(3)", "T018", "static message"),
        ("send_self(Missing)", "T018", "unknown self-send"),
        ("send_self(Get, 1)", "T018", "one-way"),
        ("send_self(Add)", "T013", "count mismatch"),
        ("send_self(Add, true)", "T005", "expected Int"),
        ("let Add = 1; send_self(Add, 1)", "T018", "shadowed"),
        (
            "let send_self = 1; send_self(Add, 1)",
            "T012",
            "not callable",
        ),
    ] {
        let text = format!(
            "process P {{ state n: Int = 0; on Start {{ {body} }} on Get(v: Int) -> Int {{ v }} on Add(v: Int) {{ n = v }} }}"
        );
        assert_error(&text, code, fragment);
    }
    for text in [
        "process P { state n: Unit = send_self(Add); on Add {} }",
        "process P { fn emit() -> Unit { send_self(Add) } state n: Int = 0; on Add {} }",
    ] {
        assert_error(text, "T018", "only supported in handlers");
    }
}

#[test]
fn process_capabilities_are_local_typed_values_and_effects_are_handler_only() {
    checked(include_str!("../../../examples/local-processes.wings"));
    checked(include_str!("../../../examples/local-process-roots.wings"));
    for (body, code, fragment) in [
        ("send(1, Add, 3)", "T005", "expected Pid"),
        ("send(self_pid(), Read)", "T018", "one-way"),
        ("send(self_pid(), Add, false)", "T005", "expected Int"),
        ("self_pid(1)", "T013", "no arguments"),
        ("spawn_self(1)", "T013", "no arguments"),
        (
            "let me = self_pid(); let same = me == me",
            "T015",
            "cannot be compared",
        ),
        ("let spawn_self = 1; spawn_self()", "T012", "not callable"),
    ] {
        let text = format!(
            "process P {{ state n: Int = 0; on Test {{ {body}; () }} on Add(value: Int) {{ n = value }} on Read -> Int {{ n }} }}"
        );
        assert_error(&text, code, fragment);
    }
    for text in [
        "process P { state me: Pid = self_pid(); on Test {} }",
        "process P { fn create() -> Pid { spawn_self() } state n: Int = 0; on Test {} }",
    ] {
        assert_error(text, "T018", "supported");
    }
    for text in [
        "process P { state n: Int = 0; on Test(value: Pid) {} }",
        "process P { state n: Int = 0; on Test -> Pid { self_pid() } }",
    ] {
        assert_error(text, "T011", "not supported in message");
    }
}

#[test]
fn language_requests_require_typed_system_completions_and_int_replies() {
    checked(include_str!("../../../examples/request-events.wings"));
    for (body, code, fragment) in [
        ("request()", "T013", "requires pid"),
        ("request(1, Get, Done, 10)", "T005", "expected Pid"),
        ("request(self_pid(), Notify, Done, 10)", "T018", "Int reply"),
        (
            "request(self_pid(), Get, Notify, 10)",
            "T018",
            "completion requires",
        ),
        (
            "request(self_pid(), Get, Done, true)",
            "T005",
            "expected Int",
        ),
        ("cancel_request(1)", "T005", "expected RequestId"),
        (
            "send_self(Done, 1, 0, 3)",
            "T018",
            "cannot be sent directly",
        ),
        (
            "let Done = 1; request(self_pid(), Get, Done, 10)",
            "T018",
            "shadowed",
        ),
        ("request(self_pid(), Get, 1, 10)", "T018", "static message"),
    ] {
        let text = format!(
            "process P {{ state count: Int = 0; on Test {{ {body}; () }} on Get -> Int {{ count }} on Notify {{ () }} on Done(id: RequestId, status: Int, value: Int) {{ () }} }}"
        );
        assert_error(&text, code, fragment);
    }
    for text in [
        "process P { state id: RequestId = request(); on Test {} }",
        "process P { fn request() -> Unit { () } state count: Int = 0; on Test {} }",
        "process P { state count: Int = 0; on Wrong(id: RequestId) {} }",
        "process P { state count: Int = 0; on Wrong -> RequestId { request() } }",
    ] {
        assert!(!analyze(text).diagnostics.is_empty(), "{text}");
    }
}

#[test]
fn while_requires_bool_condition_unit_body_and_local_scope() {
    checked(
        "process P { state n: Int = 0; on Count(limit: Int) -> Int { let step = 1; while n < limit { let next = n + step; n = next }; n } }",
    );
    checked("process P { state flag: Bool = true; on Work { while flag { flag = false } } }");
    checked("process P { state unit: Unit = (); on Work { while false { unit = () } } }");
    for (body, code, fragment) in [
        ("while 1 {}; 0", "T005", "expected Bool"),
        ("while false { 1 }; 0", "T005", "expected Unit"),
        (
            "while false { let inside = 1 }; inside",
            "T006",
            "unknown name",
        ),
        (
            "let outer = 1; while false { let outer = 2 }; 0",
            "T004",
            "duplicate name",
        ),
        (
            "let outer = 1; while false { outer = 2 }; 0",
            "T008",
            "only state",
        ),
    ] {
        assert_error(&handler(body), code, fragment);
    }
    checked(
        "process P { state n: RecordInt = record_int(0); on Work { while read_int(n) < 10 { n = record_int(read_int(n) + 1) } } }",
    );
}

#[test]
fn lists_have_distinct_types_across_builtins_state_functions_and_messages() {
    let text = "process P { fn keep(values: IntList) -> IntList { collect(); values } state values: IntList = empty_ints(); on Work { values = prepend_int(7, keep(values)); while is_empty_ints(values) == false { values = tail_ints(values) } } }";
    checked(text);
    for (body, code, fragment) in [
        ("head_int(record_int(7))", "T005", "expected IntList"),
        ("read_int(empty_ints())", "T005", "expected RecordInt"),
        ("prepend_int(true, empty_ints()); 0", "T005", "expected Int"),
        ("prepend_int(1); 0", "T013", "expects 2"),
        ("empty_ints(1); 0", "T013", "expects 0"),
        (
            "let same = empty_ints() == empty_ints(); 0",
            "T015",
            "cannot be compared",
        ),
        (
            "let values = if true { empty_ints() } else { record_int(7) }; 0",
            "T005",
            "expected IntList",
        ),
    ] {
        assert_error(&handler(body), code, fragment);
    }
    checked(
        "process P { state n: Int = 0; on Work(value: IntList) { collect(); let first = is_empty_ints(value) } }",
    );
    assert_error(
        "process P { state n: Int = 0; on Get -> IntList { empty_ints() } }",
        "T011",
        "IntList is not supported",
    );
}

#[test]
fn functions_share_types_and_references_without_process_state_or_recursion() {
    let text = "process P { state n: RecordInt = make(0); fn make(value: Int) -> RecordInt { record_int(value) } fn next(value: RecordInt) -> RecordInt { collect(); make(read_int(value) + 1) } on Count -> Int { n = next(n); read_int(n) } }";
    let analysis = checked(text);
    assert_eq!(
        analysis
            .symbol_at(text.rfind("next(n)").unwrap())
            .unwrap()
            .kind,
        SymbolKind::Function
    );
    for (definition, code, fragment) in [
        ("fn f(x: Int) -> Int { f(x) }", "T012", "unsupported"),
        (
            "fn f() -> Int { g() } fn g() -> Int { 1 }",
            "T012",
            "unsupported",
        ),
        ("fn f() -> Int { n }", "T006", "unknown name"),
        ("fn f(x: Int) -> Int { x = 2; x }", "T008", "only state"),
        ("fn f() -> Unit { while false {} }", "T017", "while"),
        ("fn f() -> Int { true }", "T005", "expected Int"),
        ("fn collect() -> Int { 1 }", "T004", "reserved"),
        (
            "fn f() -> Int { 1 } fn f() -> Int { 2 }",
            "T004",
            "duplicate",
        ),
        (
            "fn f(x: Int) -> Int { x } fn g() -> Int { f(true) }",
            "T005",
            "expected Int",
        ),
        (
            "fn f(x: Int) -> Int { x } fn g() -> Int { f() }",
            "T013",
            "expects 1",
        ),
        (
            "fn f(x: Int) -> Int { x } fn g(f: Int) -> Int { f(1) }",
            "T012",
            "not callable",
        ),
    ] {
        assert_error(
            &format!("process P {{ state n: Int = 0; {definition} }}"),
            code,
            fragment,
        );
    }
    for count in [32, 33] {
        let mut definitions = String::from("fn f0(x: Int) -> Int { x }");
        for index in 1..count {
            definitions.push_str(&format!(
                " fn f{index}(x: Int) -> Int {{ f{}(x) }}",
                index - 1
            ));
        }
        let text = format!("process P {{ state n: Int = 0; {definitions} }}");
        if count == 32 {
            checked(&text);
        } else {
            assert_error(&text, "T017", "exceeds 32");
        }
    }
    checked(
        "process P { state n: Int = f(1); fn f(value: Int) -> Int { value } fn g(value: Int) -> Bool { value == 1 } on Get -> Int { let f = 2; f } }",
    );
    assert_error(
        "process P { state n: Int = 0; fn f() -> Int { 1 } on Get(f: Int) -> Int { f() } }",
        "T012",
        "not callable",
    );
    assert_error(
        "process P { state n: Int = 0; fn f(x: Int, x: Int) -> Int { x } }",
        "T004",
        "duplicate",
    );
    assert_error(
        "process P { state n: Int = 0; fn f(x: Int) -> Int { x } on Get -> Int { f(1, 2) } }",
        "T013",
        "expects 1",
    );
}

#[test]
fn comparisons_require_same_scalar_equality_or_signed_integer_ordering() {
    checked(
        "process P { state n: Int = 0; on Get(left: Int, right: Int) -> Bool { let less: Bool = left < right; if less { left != right } else { left >= right } } on Eq(left: Bool) -> Bool { left == true } on Unit -> Bool { () != () } }",
    );
    for source in [
        "true == 1",
        "() != false",
        "1 < true",
        "false >= true",
        "1 < 2 < 3",
    ] {
        assert_error(
            &handler(&format!("let result = {source}; 0")),
            "T005",
            "expected",
        );
    }
    assert_error(
        &handler("let result = record_int(1) == record_int(1); 0"),
        "T015",
        "cannot be compared",
    );
}

#[test]
fn conditional_types_follow_bool_conditions_and_same_typed_arms() {
    checked(
        "process P { state n: Int = if true { 7 } else { 8 }; on Get(flag: Bool) -> Int { let selected = if flag { read_int(record_int(n)) } else { 9 }; selected } }",
    );
    for (body, code, fragment) in [
        ("if 1 { 2 } else { 3 }", "T005", "expected Bool"),
        ("if true { 1 } else { false }", "T005", "expected Int"),
        (
            "if true { record_int(1) } else { 2 }",
            "T005",
            "expected RecordInt",
        ),
        ("if true { 1 } else { missing }", "T006", "unknown name"),
    ] {
        assert_error(&handler(body), code, fragment);
    }
    let text = "process P { state n: Int = 0; on Get(flag: Bool) -> Bool { if flag { false } else { true } } on Unit(flag: Bool) -> Unit { if flag { () } else { collect() } } }";
    checked(text);
    checked(
        "process P { state n: RecordInt = if true { record_int(1) } else { record_int(2) }; on Get(flag: Bool) -> Int { n = if flag { record_int(7) } else { n }; let selected = if flag { n } else { if true { record_int(9) } else { n } }; read_int(selected) } }",
    );
}

#[test]
fn record_builtins_infer_locals_and_resolve_aliases() {
    let text = handler(
        "let saved: RecordInt = record_int(value); let alias = saved; collect(); read_int(alias)",
    );
    let analysis = checked(&text);
    let symbol = analysis
        .symbols
        .iter()
        .find(|symbol| symbol.name == "alias")
        .unwrap();
    assert_eq!(symbol.ty, Type::RecordInt);
    assert_eq!(symbol.ty.to_string(), "RecordInt");
    let reference = text.rfind("alias").unwrap();
    assert_eq!(analysis.symbol_at(reference).unwrap().span, symbol.span);
}

#[test]
fn rejects_wrong_builtin_names_arity_types_and_shadowed_calls() {
    for (body, code, fragment) in [
        ("record_int(true); 0", "T005", "expected Int"),
        ("read_int(1)", "T005", "expected RecordInt"),
        ("collect(1); 0", "T013", "expects 0"),
        ("record_int(); 0", "T013", "expects 1"),
        ("record_int(1, 2); 0", "T013", "found 2"),
        ("request(1); 0", "T013", "requires pid"),
        ("let collect = 1; collect(); 0", "T012", "shadows a builtin"),
        (
            "let saved = record_int(1); saved = record_int(2); 0",
            "T008",
            "only state",
        ),
        ("count = record_int(1); 0", "T005", "found RecordInt"),
        ("record_int(1)", "T005", "found RecordInt"),
    ] {
        assert_error(&handler(body), code, fragment);
    }
}

#[test]
fn accepts_record_state_and_message_fields_but_rejects_reference_replies() {
    checked(
        "process P { state n: RecordInt = record_int(0); on Get -> Int { collect(); read_int(n) } on Put(value: Int) { n = record_int(value); collect() } }",
    );
    assert_error(
        "process P { state n: RecordInt = 0 }",
        "T005",
        "expected RecordInt",
    );
    assert_error(
        "process P { state n: RecordInt = record_int(0); on Put { n = 1 } }",
        "T005",
        "expected RecordInt",
    );
    for source in [
        "process P { state n: Int = 0; on Get -> IntList { empty_ints() } }",
        "process P { state n: Int = 0; on Get -> RecordInt { record_int(1) } }",
    ] {
        assert_error(source, "T011", "not supported in message");
    }
    checked("process P { state n: Int = read_int(record_int(7)); on Get -> Int { n } }");
    checked(
        "process P { state n: Int = 0; on Put(value: RecordInt) { collect(); n = read_int(value) } on Get(value: RecordInt) -> Int { collect(); read_int(value) } }",
    );
}

#[test]
fn counter_checks_and_preserves_typed_message_protocol() {
    let text = "process Counter {\n state count: Int = 0\n on Inc(by: Int) -> Int { let next = count + by; count = next; next }\n on Reset { count = 0 }\n on Confirm -> Unit {}\n}";
    let analysis = checked(text);
    let inc = analysis.symbols.iter().find(|s| s.name == "Inc").unwrap();
    assert_eq!(inc.kind, SymbolKind::Handler);
    assert_eq!(
        inc.ty,
        Type::Message {
            parameters: vec![("by".into(), Type::Int)],
            reply: Some(Box::new(Type::Int))
        }
    );
    let local = analysis.symbols.iter().find(|s| s.name == "next").unwrap();
    assert_eq!(local.ty, Type::Int);
    let reset = analysis.symbols.iter().find(|s| s.name == "Reset").unwrap();
    let confirm = analysis
        .symbols
        .iter()
        .find(|s| s.name == "Confirm")
        .unwrap();
    assert_eq!(
        reset.ty,
        Type::Message {
            parameters: vec![],
            reply: None
        }
    );
    assert_eq!(
        confirm.ty,
        Type::Message {
            parameters: vec![],
            reply: Some(Box::new(Type::Unit))
        }
    );
}

#[test]
fn bool_unit_and_annotated_locals_infer_expected_types() {
    let analysis = checked(
        "process Flag { state enabled: Bool = false; on Set(value: Bool) -> Bool { let previous: Bool = enabled; enabled = value; previous } on Clear { enabled = false } on Ack -> Unit { let x = (); x } }",
    );
    for (name, expected) in [
        ("enabled", Type::Bool),
        ("previous", Type::Bool),
        ("x", Type::Unit),
    ] {
        assert_eq!(
            analysis.symbols.iter().find(|s| s.name == name).unwrap().ty,
            expected
        );
    }
}

#[test]
fn type_display_distinguishes_requests_and_one_way_messages() {
    for (ty, expected) in [
        (Type::Int, "Int"),
        (Type::Bool, "Bool"),
        (Type::Unit, "Unit"),
        (Type::Unknown, "unknown"),
        (
            Type::Message {
                parameters: vec![],
                reply: None,
            },
            "() (one-way)",
        ),
        (
            Type::Message {
                parameters: vec![],
                reply: Some(Box::new(Type::Unit)),
            },
            "() -> Unit",
        ),
        (
            Type::Message {
                parameters: vec![("by".into(), Type::Int), ("enabled".into(), Type::Bool)],
                reply: Some(Box::new(Type::Int)),
            },
            "(by: Int, enabled: Bool) -> Int",
        ),
    ] {
        assert_eq!(ty.to_string(), expected);
    }
}

#[test]
fn exactly_one_state_is_required() {
    assert_error("process P { on Get {} }", "T001", "exactly one state");
    assert_error(
        "process P { state a: Int = 0; state b: Int = 1 }",
        "T001",
        "exactly one state",
    );
    checked("process P { state n: Unit = () }");
}

#[test]
fn duplicate_processes_handlers_states_parameters_and_locals_are_rejected() {
    for text in [
        "process P { state n: Int = 0 } process P { state n: Int = 0 }",
        "process P { state n: Int = 0; on Get {} on Get -> Int { 1 } }",
        "process P { state n: Int = 0; state n: Int = 1 }",
        "process P { state n: Int = 0; on Get(x: Int, x: Bool) {} }",
        "process P { state n: Int = 0; on Get { let x = 1; let x = 2; } }",
    ] {
        assert_error(text, "T004", "duplicate name");
    }
}

#[test]
fn parameters_and_locals_cannot_shadow_state_or_each_other() {
    for text in [
        "process P { state n: Int = 0; on Get(n: Int) {} }",
        "process P { state n: Int = 0; on Get { let n = 1; } }",
        "process P { state n: Int = 0; on Get(x: Int) { let x = 1; } }",
    ] {
        assert_error(text, "T004", "duplicate name");
    }
}

#[test]
fn independent_process_and_handler_scopes_can_reuse_names() {
    checked(
        "process P { state n: Int = 0; on Get(x: Int) -> Int { let y = x; y } on Set(x: Int) { let y = x; n = y } } process Q { state n: Bool = false; on Get(x: Bool) -> Bool { let y = x; y } }",
    );
}

#[test]
fn initial_state_cannot_reference_itself_other_states_or_handlers() {
    for text in [
        "process P { state n: Int = n }",
        "process P { state a: Int = 0; state b: Int = a }",
        "process P { state n: Int = value; on Set(value: Int) {} }",
    ] {
        assert_error(text, "T006", "unknown name");
    }
}

#[test]
fn local_names_only_resolve_after_their_initializers() {
    assert_error(&handler("let x = x; x"), "T006", "unknown name 'x'");
    assert_error(
        &handler("let x = y; let y = 1; x"),
        "T006",
        "unknown name 'y'",
    );
    checked(&handler("let x = value; let y = x + count; y"));
}

#[test]
fn locals_and_parameters_do_not_leak_between_handlers() {
    let text = "process P { state n: Int = 0; on First(x: Int) { let y = x; } on Second -> Int { x + y } }";
    let analysis = analyze(text);
    assert_eq!(
        analysis
            .diagnostics
            .iter()
            .filter(|d| d.code == "T006")
            .count(),
        2
    );
    let offset = text.rfind("x + y").unwrap();
    let visible: Vec<_> = analysis
        .visible_symbols(offset)
        .map(|s| s.name.as_str())
        .collect();
    assert!(!visible.contains(&"x"));
    assert!(!visible.contains(&"y"));
}

#[test]
fn only_state_bindings_can_be_assigned() {
    assert_error(&handler("value = 2; value"), "T008", "only state bindings");
    assert_error(
        &handler("let x = 1; x = 2; x"),
        "T008",
        "only state bindings",
    );
    assert_error(
        &handler("missing = 2; count"),
        "T006",
        "unknown name 'missing'",
    );
    checked(&handler("count = value; count"));
}

#[test]
fn type_mismatches_cover_initializers_assignments_and_local_annotations() {
    for text in [
        "process P { state n: Int = false }",
        "process P { state n: Int = 0; on Set { n = true } }",
        "process P { state n: Int = 0; on Set { let x: Bool = 1; } }",
    ] {
        assert_error(text, "T005", "expected");
    }
}

#[test]
fn unknown_types_are_reported_at_every_annotation_site() {
    for text in [
        "process P { state n: Missing = 0 }",
        "process P { state n: Int = 0; on Get(x: Missing) {} }",
        "process P { state n: Int = 0; on Get -> Missing {} }",
        "process P { state n: Int = 0; on Get { let x: Missing = 1; } }",
    ] {
        assert_error(text, "T002", "unknown type 'Missing'");
    }
}

#[test]
fn all_arithmetic_operators_require_int_without_implicit_conversion() {
    for expression in ["true + 1", "1 - false", "true * false", "true / 2", "-true"] {
        assert_error(&handler(expression), "T005", "expected Int, found Bool");
    }
    checked(&handler("-(count + value) * 2 / 3 - 1"));
}

#[test]
fn reply_tail_must_match_declared_type_and_one_way_requires_unit() {
    for text in [
        "process P { state n: Int = 0; on Get -> Int {} }",
        "process P { state n: Int = 0; on Get -> Int { true } }",
        "process P { state n: Int = 0; on Get { n } }",
        "process P { state n: Int = 0; on Get -> Unit { 1 } }",
        "process P { state n: Int = 0; on Get -> Int { 1; } }",
    ] {
        assert_error(text, "T005", "expected");
    }
    checked("process P { state n: Int = 0; on Get { n; } on Ack -> Unit {} on Read -> Int { n } }");
}

#[test]
fn message_names_must_start_with_uppercase_letters() {
    for name in ["get", "_Get", "读取"] {
        assert_error(
            &format!("process P {{ state n: Int = 0; on {name} {{}} }}"),
            "T010",
            "uppercase",
        );
    }
    checked("process P { state n: Int = 0; on Événement {} }");
}

#[test]
fn symbol_queries_resolve_declarations_references_and_assignment_targets() {
    let text = handler("let next = count + value; count = next; next");
    let analysis = checked(&text);
    for reference in &analysis.references {
        let symbol = &analysis.symbols[reference.symbol];
        assert_eq!(&text[reference.span.start..reference.span.end], symbol.name);
        for offset in reference.span.start..reference.span.end {
            assert_eq!(analysis.symbol_at(offset).unwrap().span, symbol.span);
        }
    }
    for symbol in &analysis.symbols {
        assert_eq!(
            analysis.symbol_at(symbol.span.start).unwrap().kind,
            symbol.kind
        );
        assert_eq!(
            analysis.symbol_at(symbol.span.start).unwrap().span,
            symbol.span
        );
    }
    let state_assignment = text.rfind("count =").unwrap();
    assert_eq!(
        analysis.symbol_at(state_assignment).unwrap().kind,
        SymbolKind::State
    );
    let local_use = text.rfind("next").unwrap();
    assert_eq!(
        analysis.symbol_at(local_use).unwrap().kind,
        SymbolKind::Local
    );
    assert!(analysis.symbol_at(text.len()).is_none());
    assert!(analysis.symbol_at(text.find('{').unwrap()).is_none());
}

#[test]
fn unicode_references_and_diagnostics_use_utf8_byte_spans() {
    let text = "process 计数器 { state 数值: Int = 0; on Get(增量: Int) -> Int { 数值 = 数值 + 增量; 数值 } }";
    let analysis = checked(text);
    let last = text.rfind("数值").unwrap();
    let symbol = analysis.symbol_at(last).unwrap();
    assert_eq!(symbol.name, "数值");
    assert_eq!(&text[symbol.span.start..symbol.span.end], "数值");
    let bad = "process 计数器 { state 数值: Int = false }";
    let analysis = analyze(bad);
    let error = analysis
        .diagnostics
        .iter()
        .find(|d| d.code == "T005")
        .unwrap();
    assert_eq!(&bad[error.span.start..error.span.end], "false");
}

#[test]
fn completion_visibility_tracks_local_declaration_order_and_process_boundaries() {
    let text = "process P { state count: Int = 0; on Get(value: Int) -> Int { let x = value; let y = x; y } } process Q { state flag: Bool = false; on Read -> Bool { flag } }";
    let analysis = checked(text);
    let before = text.find("let x").unwrap();
    let after_x = text.find("let y").unwrap();
    let after_y = text.find("; y }").unwrap() + 2;
    let names = |offset| {
        analysis
            .visible_symbols(offset)
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
    };
    assert!(names(before).contains(&"value"));
    assert!(!names(before).contains(&"x"));
    assert!(names(after_x).contains(&"x"));
    assert!(!names(after_x).contains(&"y"));
    assert!(names(after_y).contains(&"y"));
    assert!(!names(after_y).contains(&"flag"));
    let other_process = text.rfind("flag }").unwrap();
    assert!(!names(other_process).contains(&"count"));
    assert!(!names(other_process).contains(&"value"));
    assert!(names(other_process).contains(&"flag"));
    assert!(names(other_process).contains(&"P"));
    assert!(names(other_process).contains(&"Q"));
}

#[test]
fn unknown_name_does_not_cascade_into_spurious_type_mismatches() {
    let analysis = analyze(&handler("missing"));
    assert_eq!(analysis.diagnostics.len(), 1);
    assert_eq!(analysis.diagnostics[0].code, "T006");
    let analysis = analyze("process P { state n: Missing = false }");
    assert_eq!(analysis.diagnostics.len(), 1);
    assert_eq!(analysis.diagnostics[0].code, "T002");
}

#[test]
fn syntax_errors_are_preserved_and_later_handlers_remain_queryable() {
    let text = "process P { state n: Int = 0; on Bad -> Int { + } on Good -> Int { n } }";
    let analysis = analyze(text);
    assert!(analysis.diagnostics.iter().any(|d| d.code == "S003"));
    let good = analysis.symbols.iter().find(|s| s.name == "Good").unwrap();
    assert_eq!(good.kind, SymbolKind::Handler);
    let last_reference = text.rfind("n }").unwrap();
    assert_eq!(
        analysis.symbol_at(last_reference).unwrap().kind,
        SymbolKind::State
    );
}

#[test]
fn analysis_diagnostic_budget_includes_syntax_and_semantic_errors() {
    let handlers = (0..200)
        .map(|i| format!("on Bad{i} -> Bool {{ 1 }}"))
        .collect::<String>();
    let analysis = analyze(&format!("process P {{ state n: Int = 0; {handlers} }}"));
    assert_eq!(analysis.diagnostics.len(), MAX_DIAGNOSTICS);
    assert!(analysis.diagnostics.iter().all(|d| d.code == "T005"));
    assert_eq!(
        analysis
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Handler)
            .count(),
        200
    );
    let analysis = analyze(&"@".repeat(MAX_DIAGNOSTICS + 10));
    assert_eq!(analysis.diagnostics.len(), MAX_DIAGNOSTICS);
    assert!(analysis.diagnostics.iter().all(|d| d.code == "S001"));
}
