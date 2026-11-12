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
