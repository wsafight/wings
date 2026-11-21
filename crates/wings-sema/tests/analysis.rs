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
    format!("actor P {{ state count = 0; on get(value: int) -> int {{ {body} }} }}")
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
fn struct_messages_and_replies_preserve_nominal_completion_types() {
    checked(
        r#"
        actor P {
          struct A { text: str }
          state n = 0
          on store(value: A) { n = value.text.len() }
          on start { self ! store(A { text: "中" }) }
          on read(value: A) -> int => value.text.len()
          on reply done(id: req, status: int, result: int) {}
          on ask_it { let _ = self.ask(read(A("😀")), timeout: 1_000, then: done) }
        }
        "#,
    );
    checked(
        r#"
        actor P {
          struct A {}
          state n = 0
          on read -> A => A {}
          on reply done(id: req, status: int, result: A) {}
          on ask_it { let _ = self.ask(read, timeout: 1_000, then: done) }
        }
        "#,
    );
    for source in [
        "actor P { struct A {} struct B {} state n = 0; on store(value: A) {} on start { self ! store(B {}) } }",
        "actor P { struct A {} struct B {} state n = 0; on read -> A => B {} }",
        "actor P { struct A {} struct B {} state n = 0; on read -> A => A {} on reply done(id: req, status: int, result: B) {} on ask_it { let _ = self.ask(read, timeout: 1_000, then: done) } }",
    ] {
        assert!(!analyze(source).diagnostics.is_empty(), "{source}");
    }
}

#[test]
fn string_concat_interpolation_and_equality_are_strictly_typed() {
    checked(
        r#"
        actor P {
          state text = "中😀"
          fn equal(left: str, right: str) -> bool => left == right
          on same(value: str) -> bool => equal(text, value)
          on join(n: int, ok: bool) -> str {
            text += "!"
            f"{text}:{n}:{ok}" + "${n}"
          }
        }
        "#,
    );
    for expression in [
        "text == 1",
        "text != b\"\"",
        "text < text",
        "text >= text",
        "text + 1",
        "f\"{b\\\"a\\\"}\"",
    ] {
        let source = format!(
            "actor P {{ state text = \"\"; on check_it -> bool {{ {expression}; true }} }}"
        );
        assert!(!analyze(&source).diagnostics.is_empty(), "{source}");
    }
}

#[test]
fn byte_methods_and_literals_require_explicit_operands() {
    checked(
        r#"
        actor P {
          fn changed(value: bytes, number: int) -> bytes => value.set_i64(1, number)
          state data = bytes::fill(16, 0)
          on set_num(number: int) -> int {
            let old = b"abcd"
            data = changed(data, number).set(0, 255)
            gc()
            data.i64_at(1) + old.len() + old.at(0)
          }
        }
        "#,
    );
    for text in [
        "actor P { state data = b\"01234567\"; on read -> int => data.i64_at(true) }",
        "actor P { state data = b\"01234567\"; on read -> int { data.set_i64(0, false); 0 } }",
        "actor P { state data = b\"01234567\"; on read -> int => data.i64_at() }",
        "actor P { state data = b\"a\"; on read -> bool => data == data }",
        "actor P { state data: bytes = []; on read -> int => data.len() }",
        "actor P { state data = bytes::fill(1, true); on read -> int => data.len() }",
    ] {
        assert!(
            !analyze(text).diagnostics.is_empty(),
            "unexpected acceptance: {text}"
        );
    }
}

#[test]
fn send_ask_and_process_capabilities_enforce_handler_protocols_and_scopes() {
    checked(include_str!("../../../examples/local-processes.wings"));
    checked(include_str!("../../../examples/local-process-roots.wings"));
    checked(include_str!("../../../examples/request-events.wings"));

    let text = "actor P { state n = 0; on start { self ! add(1, true, ()) } on add(v: int, b: bool, u: ()) { n = v } }";
    let analysis = checked(text);
    let target = text.find("add(1").unwrap();
    let reference = analysis
        .references
        .iter()
        .find(|reference| reference.span.start == target)
        .unwrap();
    assert_eq!(analysis.symbols[reference.symbol].kind, SymbolKind::Handler);

    for (body, code, fragment) in [
        ("1 ! add(3)", "T005", "expected pid"),
        ("self ! missing", "T018", "unknown message"),
        ("self ! read", "T018", "one-way"),
        ("self ! done(1, 0, 0)", "T018", "on reply"),
        ("self ! add", "T013", "expects 1 argument"),
        ("self ! add(false)", "T005", "expected int"),
        (
            "1.ask(read, timeout: 10, then: done)",
            "T005",
            "expected pid",
        ),
        (
            "self.ask(add(1), timeout: 10, then: done)",
            "T018",
            "returns '-> type'",
        ),
        ("self.ask(read, timeout: 10, then: add)", "T018", "on reply"),
        (
            "self.ask(read, timeout: false, then: done)",
            "T005",
            "expected int",
        ),
        (
            "let id = self.ask(read, timeout: 10, then: done); let same = id == id",
            "T015",
            "cannot be compared",
        ),
    ] {
        let src = format!(
            "actor P {{ state n = 0; on test_it {{ {body}; () }} on add(v: int) {{ n = v }} on read -> int => n; on reply done(id: req, status: int, val: int) {{}} }}"
        );
        assert_error(&src, code, fragment);
    }

    for src in [
        "actor P { state n: () = self ! add; on add {} }",
        "actor P { fn emit() { self ! add } on add {} }",
        "actor P { state me: pid = self; on add {} }",
        "actor P { fn create() -> pid => spawn() }",
        "actor P { fn cancel_it(id: req) => id.cancel() }",
        "actor P { fn delay_it() { send_after(10, add) } on add {} }",
        "actor P { fn watch_it(p: pid) { p.monitor(down) } on down(p: pid, r: int) {} }",
    ] {
        assert_error(src, "T018", "handlers");
    }

    for src in [
        "actor P { on test_it(value: req) {} }",
        "actor P { on test_it -> req { self.ask(read, timeout: 10, then: done) } on read -> int => 1; on reply done(id: req, status: int, val: int) {} }",
    ] {
        assert_error(src, "T011", "not supported");
    }

    checked(
        r#"
        struct PeerInfo { id: int, peer: pid<Worker> }
        actor Worker {
          state parent: pid = null
          on bind(p: pid) -> pid {
            parent = p
            self
          }
          on ping(n: int) {
            if !parent.is_null() && parent != null {
              send_after(5, ping(n))
            }
          }
        }
        actor Main {
          state worker: pid<Worker> = null
          state info: PeerInfo = PeerInfo { id: 0, peer: null }
          on start {
            worker = spawn(Worker)
            worker.monitor(on_down)
            worker.send_after(10, ping(1))
            let _ = worker.ask(bind(self), timeout: 100, then: on_bound)
          }
          on on_down(dead: pid<Worker>, reason: int) {
            if dead == worker {
              worker = null
            }
          }
          on reply on_bound(id: req, status: int, w: pid) {}
        }
        "#,
    );
}

#[test]
fn structs_support_literals_spread_deep_mutation_and_reject_cycles_or_overflow() {
    checked(
        r#"
        actor P {
          struct Leaf { count: int, name: str }
          struct Node { leaf: Leaf, ok: bool }
          state node = Node { leaf: Leaf { count: 1, name: "a" }, ok: true }
          on bump(name: str) -> int {
            let local = Node { ok: false, ..node }
            local.leaf.count += 5
            node.leaf = Leaf { name, ..local.leaf }
            node.leaf.count
          }
        }
        "#,
    );
    assert_error(
        "actor P { struct A { x: int, y: int } on f -> A => A { x: 1 } }",
        "T013",
        "missing field 'y'",
    );
    assert_error(
        "actor P { struct A { x: int } on f -> A => A { x: 1, x: 2 } }",
        "T004",
        "duplicate field 'x'",
    );
    assert_error(
        "actor P { struct A { next: B } struct B { x: int } }",
        "T002",
        "unknown type 'B'",
    );
    assert_error(
        "actor P { struct Big { a: int, b: int, c: int, d: int, e: int, f: int, g: int, h: int, i: int } }",
        "T019",
        "exceeds 8",
    );
}

#[test]
fn for_loops_and_match_enforce_types_and_exhaustiveness() {
    checked(
        r#"
        actor P {
          state sum = 0
          on calc(xs: [int], flag: bool, tag: str) -> int {
            for i in 0..10 { sum += i }
            for i in 0..=10 { sum += i }
            for x in xs { sum += x }
            let a = match flag { true => 1, false => 0 }
            let b = match xs { [] => 0, [h, ..t] => h + t.len_or_zero() }
            a
          }
        }
        "#
        .replace(".len_or_zero()", ".head()")
        .as_str(),
    );

    assert_error(
        "actor P { on f(flag: bool) -> int => match flag { true => 1 } }",
        "T005",
        "non-exhaustive match",
    );
    assert_error(
        "actor P { on f(xs: [int]) -> int => match xs { [] => 0 } }",
        "T005",
        "non-exhaustive match",
    );
    assert_error(
        "actor P { on f(n: int) -> int => match n { 0 => 0, 1 => 1 } }",
        "T005",
        "non-exhaustive match",
    );
    assert_error(
        "actor P { on f(n: int) -> int => match n { _ => 0, 1 => 1 } }",
        "T005",
        "unreachable match arm",
    );
}

#[test]
fn multi_state_allows_zero_to_eight_states_and_rejects_nine() {
    checked("actor Empty { on ping {} }");
    checked(
        r#"
        actor Multi {
          state a = 1
          state b = "two"
          state c = true
          state d: [int] = [a]
          on check_it -> int => a + d.head()
        }
        "#,
    );
    assert_error(
        "actor TooMany { state s1 = 0; state s2 = 0; state s3 = 0; state s4 = 0; state s5 = 0; state s6 = 0; state s7 = 0; state s8 = 0; state s9 = 0 }",
        "T001",
        "at most 8 state bindings",
    );
}

#[test]
fn naming_conventions_and_type_display_are_consistent() {
    assert_error("actor bad_actor { on ping {} }", "T020", "PascalCase");
    assert_error("actor P { struct bad_struct {} }", "T020", "PascalCase");
    assert_error("actor P { state Bad = 0 }", "T020", "snake_case");
    assert_error("actor P { on Bad {} }", "T020", "snake_case");
    assert_error("actor P { fn Bad() {} }", "T020", "snake_case");

    for (ty, expected) in [
        (Type::Int, "int"),
        (Type::Bool, "bool"),
        (Type::Unit, "()"),
        (Type::BoxInt, "box<int>"),
        (Type::IntList, "[int]"),
        (Type::Bytes, "bytes"),
        (Type::String, "str"),
        (Type::Pid, "pid"),
        (Type::RequestId, "req"),
        (Type::Unknown, "unknown"),
        (
            Type::Message {
                parameters: vec![],
                reply: None,
                is_reply: false,
            },
            "() (one-way)",
        ),
        (
            Type::Message {
                parameters: vec![],
                reply: Some(Box::new(Type::Unit)),
                is_reply: false,
            },
            "() -> ()",
        ),
        (
            Type::Message {
                parameters: vec![("by".into(), Type::Int), ("enabled".into(), Type::Bool)],
                reply: Some(Box::new(Type::Int)),
                is_reply: false,
            },
            "(by: int, enabled: bool) -> int",
        ),
        (
            Type::Message {
                parameters: vec![
                    ("id".into(), Type::RequestId),
                    ("status".into(), Type::Int),
                    ("value".into(), Type::Int),
                ],
                reply: None,
                is_reply: true,
            },
            "(id: req, status: int, value: int) (reply)",
        ),
    ] {
        assert_eq!(ty.to_string(), expected);
    }
}

#[test]
fn duplicate_names_scoping_assignments_and_operators_are_checked() {
    for text in [
        "actor P { state n = 0 } actor P { state n = 0 }",
        "actor P { state n = 0; on get {} on get -> int => 1 }",
        "actor P { state n = 0; state n = 1 }",
        "actor P { state n = 0; on get(x: int, x: bool) {} }",
        "actor P { state n = 0; on get { let x = 1; let x = 2; } }",
        "actor P { state n = 0; on get(n: int) {} }",
        "actor P { state n = 0; on get { let n = 1; } }",
    ] {
        assert_error(text, "T004", "duplicate name");
    }

    assert_error(&handler("let x = x; x"), "T006", "unknown name 'x'");
    assert_error(
        &handler("value = 2; value"),
        "T008",
        "only 'state' and 'let' bindings",
    );
    checked(&handler("let x = 1; x += value; count = x; count"));

    for expression in [
        "true + 1",
        "1 - false",
        "true * false",
        "true / 2",
        "10 % false",
        "-true",
        "!1",
        "1 && true",
        "false || 0",
    ] {
        assert_error(&handler(expression), "T005", "expected");
    }
    checked(&handler("-(count + value) * 2 / 3 % 5 - 1"));
}

#[test]
fn symbol_queries_utf8_spans_and_diagnostic_budget_hold() {
    let text = handler("let next = count + value; count = next; next");
    let analysis = checked(&text);
    for reference in &analysis.references {
        let symbol = &analysis.symbols[reference.symbol];
        assert_eq!(&text[reference.span.start..reference.span.end], symbol.name);
        for offset in reference.span.start..reference.span.end {
            assert_eq!(analysis.symbol_at(offset).unwrap().span, symbol.span);
        }
    }

    let unicode =
        "actor Counter { state 数值 = 0; on get(增量: int) -> int { 数值 += 增量; 数值 } }";
    let uni_analysis = checked(unicode);
    let last = unicode.rfind("数值").unwrap();
    let sym = uni_analysis.symbol_at(last).unwrap();
    assert_eq!(sym.name, "数值");

    let handlers = (0..200)
        .map(|i| format!("on bad_{i} -> bool => 1; "))
        .collect::<String>();
    let capped = analyze(&format!("actor P {{ state n = 0; {handlers} }}"));
    assert_eq!(capped.diagnostics.len(), MAX_DIAGNOSTICS);
    assert!(capped.diagnostics.iter().all(|d| d.code == "T005"));
}

#[test]
fn recursion_forward_functions_and_multi_actor_protocols_typecheck() {
    checked(
        r#"
        struct Task { id: int, label: str }
        struct Outcome { id: int, score: int }

        actor Helper {
          fn is_even(n: int) -> bool => if n == 0 { true } else { is_odd(n - 1) }
          fn is_odd(n: int) -> bool => if n == 0 { false } else { is_even(n - 1) }
          fn fact(n: int) -> int => if n <= 1 { 1 } else { n * fact(n - 1) }

          on fire(delta: int) {}
          on evaluate(task: Task) -> Outcome => Outcome {
            id: task.id,
            score: if is_even(task.id) { fact(4) } else { task.label.len() },
          }
        }

        actor Supervisor {
          state acc = 0
          fn helper_passthrough(p: pid<Helper>) -> pid<Helper> => p

          on run -> int {
            let h = helper_passthrough(spawn(Helper))
            h ! fire(1)
            h.ask(evaluate(Task { id: 2, label: "ok" }), timeout: 100, then: on_eval)
            acc
          }

          on reply on_eval(id: req, status: int, out: Outcome) {
            if status == 0 {
              acc += out.score
            }
          }
        }
        "#,
    );

    assert_error(
        "actor A { on go { let w = spawn(Missing) } }",
        "T002",
        "unknown actor 'Missing'",
    );
    assert_error(
        "actor W { on ping {} } actor M { state saved: pid<W> = spawn(W) }",
        "T018",
        "handlers",
    );
    assert_error(
        r#"
        actor W {
          struct Local { x: int }
          on take(v: Local) {}
        }
        actor M {
          struct Local { x: int, y: int }
          on go {
            let w = spawn(W)
            w ! take(Local { x: 1, y: 2 })
          }
        }
        "#,
        "T019",
        "shared top-level struct",
    );
}
