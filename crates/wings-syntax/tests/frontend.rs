use wings_source::{MAX_DIAGNOSTICS, Span};
use wings_syntax::ast::{
    BinaryOperator, Expr, ExprKind, InterpolatedPart, MatchPattern, Statement, TypeKind,
};
use wings_syntax::{MAX_SOURCE_BYTES, TokenKind, lex, parse};

fn expression(text: &str) -> Expr {
    let parsed = parse(&format!(
        "actor P {{ state n = 0; on get -> int {{ {text} }} }}"
    ));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    *parsed.program.actors[0].handlers[0]
        .body
        .tail
        .clone()
        .unwrap()
}

#[test]
fn string_literals_decode_utf8_escapes_and_preserve_lossless_tokens() {
    for (source, expected) in [
        (r#""""#, ""),
        (r#""中文😀""#, "中文😀"),
        (r#""\"\\\n\r\t\0#{}""#, "\"\\\n\r\t\0#{}"),
    ] {
        let ExprKind::String(value) = expression(source).kind else {
            panic!("missing string")
        };
        assert_eq!(value, expected);
        let (tokens, diagnostics) = lex(source);
        assert!(diagnostics.is_empty());
        assert_eq!(tokens[0].kind, TokenKind::String);
        assert_eq!(tokens[0].span, Span::new(0, source.len()));
        assert_eq!(
            tokens
                .iter()
                .map(|token| &source[token.span.start..token.span.end])
                .collect::<String>(),
            source
        );
    }
}

#[test]
fn string_interpolation_and_byte_literals_parse_cleanly() {
    let ExprKind::InterpolatedString(parts) = expression(r#"f"hi {n + 1} {{lit}}""#).kind else {
        panic!("expected f-string interpolation")
    };
    assert_eq!(parts.len(), 3);
    assert!(matches!(&parts[0], InterpolatedPart::Literal(s) if s == "hi "));
    assert!(matches!(&parts[1], InterpolatedPart::Expr(_)));
    assert!(matches!(&parts[2], InterpolatedPart::Literal(s) if s == " {lit}"));

    let ExprKind::InterpolatedString(dollar_parts) = expression(r#""val=${n}""#).kind else {
        panic!("expected dollar interpolation")
    };
    assert_eq!(dollar_parts.len(), 2);

    let ExprKind::Bytes(bytes) = expression(r#"b"AB\x00\xff\n""#).kind else {
        panic!("expected byte literal")
    };
    assert_eq!(bytes, vec![b'A', b'B', 0x00, 0xff, b'\n']);
}

#[test]
fn string_literal_errors_are_bounded_and_recover_at_following_handlers() {
    for literal in [
        r#""\q""#,
        r#""\u1234""#,
        "\"unterminated",
        "\"raw\tcontrol\"",
        "\"dangling\\",
        r#"f"unclosed {n""#,
        r#"f"empty {}""#,
        r#"b"\xGG""#,
    ] {
        let source =
            format!("actor P {{ state text: str = {literal}\non final_h -> int {{ 7 }} }}");
        let parsed = parse(&source);
        assert!(
            parsed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "S005"),
            "{literal}: {:?}",
            parsed.diagnostics
        );
        assert!(
            parsed.program.actors[0]
                .handlers
                .iter()
                .any(|handler| handler.name.text == "final_h"),
            "{literal}"
        );
        assert!(parsed.diagnostics.len() <= MAX_DIAGNOSTICS);
    }
}

#[test]
fn string_literal_limit_counts_decoded_utf8_bytes_not_source_characters() {
    for (body, valid) in [
        ("a".repeat(65536), true),
        ("a".repeat(65537), false),
        ("😀".repeat(16384), true),
        ("😀".repeat(16385), false),
        (r"\0".repeat(65536), true),
    ] {
        let source = format!("actor P {{ state text: str = \"{body}\"; }}");
        let parsed = parse(&source);
        assert_eq!(parsed.diagnostics.is_empty(), valid);
        if valid {
            let ExprKind::String(value) = &parsed.program.actors[0].states[0].default.kind else {
                panic!("missing literal")
            };
            assert_eq!(value.len(), 65536);
        }
    }
}

#[test]
fn functions_parse_signatures_fat_arrows_and_recover_before_following_handlers() {
    let parsed = parse(
        "actor P { state n = twice(1); fn twice(value: int) -> int => value + value; on get -> int => twice(n) }",
    );
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let function = &parsed.program.actors[0].functions[0];
    assert_eq!(function.name.text, "twice");
    assert_eq!(function.parameters[0].name.text, "value");
    assert!(matches!(
        &function.result.as_ref().unwrap().kind,
        TypeKind::Named(name) if name.text == "int"
    ));
    for declaration in [
        "fn broken(value: ) -> int { value }",
        "fn broken() -> int { let value = on get -> int { 7 }",
    ] {
        let parsed = parse(&format!(
            "actor P {{ state n = 0; {declaration} on final_h -> int {{ n }} }}"
        ));
        assert!(!parsed.diagnostics.is_empty(), "{declaration}");
        assert!(
            parsed.program.actors[0]
                .handlers
                .iter()
                .any(|handler| handler.name.text == "final_h")
        );
    }
    for count in [64, 65] {
        let functions: String = (0..count)
            .map(|index| format!("fn f{index}() -> int {{ 0 }} "))
            .collect();
        let parsed = parse(&format!("actor P {{ state n = 0; {functions} }}"));
        assert_eq!(parsed.diagnostics.is_empty(), count == 64);
        assert_eq!(parsed.program.actors[0].functions.len(), count.min(64));

        let parameters = (0..count)
            .map(|index| format!("p{index}: int"))
            .collect::<Vec<_>>()
            .join(", ");
        let parsed = parse(&format!(
            "actor P {{ state n = 0; fn many({parameters}) -> int {{ 0 }} }}"
        ));
        assert_eq!(parsed.diagnostics.is_empty(), count == 64);
    }
}

fn shape(expression: &Expr) -> String {
    match &expression.kind {
        ExprKind::Integer(value) => value.to_string(),
        ExprKind::Name(name) => name.text.clone(),
        ExprKind::Negate(value) => format!("(-{})", shape(value)),
        ExprKind::Not(value) => format!("(!{})", shape(value)),
        ExprKind::Call { callee, arguments } => {
            let args: Vec<_> = arguments.iter().map(shape).collect();
            format!("{}({})", callee.text, args.join(", "))
        }
        ExprKind::Send {
            target,
            message,
            arguments,
        } => {
            let args: Vec<_> = arguments.iter().map(shape).collect();
            format!(
                "({} ! {}({}))",
                shape(target),
                message.text,
                args.join(", ")
            )
        }
        ExprKind::Binary {
            operator,
            left,
            right,
        } => {
            let operator = match operator {
                BinaryOperator::Add => "+",
                BinaryOperator::Subtract => "-",
                BinaryOperator::Multiply => "*",
                BinaryOperator::Divide => "/",
                BinaryOperator::Remainder => "%",
                BinaryOperator::Equal => "==",
                BinaryOperator::NotEqual => "!=",
                BinaryOperator::Less => "<",
                BinaryOperator::LessEqual => "<=",
                BinaryOperator::Greater => ">",
                BinaryOperator::GreaterEqual => ">=",
                BinaryOperator::And => "&&",
                BinaryOperator::Or => "||",
            };
            format!("({} {operator} {})", shape(left), shape(right))
        }
        other => panic!("unexpected expression {other:?}"),
    }
}

#[test]
fn structs_lists_for_match_and_condition_blocks_parse_without_ambiguity() {
    let parsed = parse(
        r#"
        actor Demo {
          struct Item { code: int, label: str }
          state item = Item { code: 1, label: "ok" }
          state xs: [int] = [1, 2]

          on work(Flag: bool, Items: [int]) -> int {
            let code = 9
            let next = Item { code, ..item }
            if Flag {
              item.code += 1
            } else if !Flag {
              item.code -= 1
            }
            while Flag {
              break_noop()
            }
            for i in 0..=3 {
              xs = [i, ..xs]
            }
            for v in Items {
              item.code += v
            }
            match Items {
              [] => 0,
              [head, ..tail] => head + next.code,
            }
          }
        }
        "#,
    );
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let handler = &parsed.program.actors[0].handlers[0];
    assert_eq!(handler.body.statements.len(), 6);
    assert!(matches!(handler.body.statements[2], Statement::Expr(_)));
    assert!(matches!(
        handler.body.statements[3],
        Statement::While { .. }
    ));
    assert!(matches!(
        handler.body.statements[4],
        Statement::ForRange {
            inclusive: true,
            ..
        }
    ));
    assert!(matches!(
        handler.body.statements[5],
        Statement::ForList { .. }
    ));
    let ExprKind::Match { arms, .. } = &handler.body.tail.as_ref().unwrap().kind else {
        panic!("expected tail match")
    };
    assert!(matches!(arms[0].pattern, MatchPattern::EmptyList(_)));
    assert!(matches!(arms[1].pattern, MatchPattern::ConsList { .. }));
}

#[test]
fn actor_send_ask_and_reply_handlers_parse_cleanly() {
    let parsed = parse(
        r#"
        actor Messenger {
          state count = 0
          on start(timeout: int) -> req {
            let child = spawn()
            child ! notify(1, 2)
            self ! ping
            child.ask(compute(7), timeout: timeout, then: on_done)
          }
          on notify(a: int, b: int) => count = a + b
          on ping => count += 1
          on compute(x: int) -> int => x * 2
          on reply on_done(id: req, status: int, value: int) {
            count = status + value
          }
        }
        "#,
    );
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let handlers = &parsed.program.actors[0].handlers;
    assert_eq!(handlers.len(), 5);
    assert!(!handlers[0].is_reply);
    assert!(handlers[4].is_reply);
    assert!(matches!(
        handlers[0].body.tail.as_ref().unwrap().kind,
        ExprKind::Ask { .. }
    ));
}

#[test]
fn struct_fields_and_methods_parse_with_postfix_precedence_and_bounded_recovery() {
    let parsed = parse(
        r#"actor P { struct A { number: int, text: str, } state value = A { number: 1, text: "中" }; on read -> int { value.number + A(2, "").text.len() } }"#,
    );
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.program.actors[0].structs[0].fields.len(), 2);
    let ExprKind::Binary { left, right, .. } = &parsed.program.actors[0].handlers[0]
        .body
        .tail
        .as_ref()
        .unwrap()
        .kind
    else {
        panic!("missing addition")
    };
    assert!(matches!(left.kind, ExprKind::Field { .. }));
    assert!(matches!(right.kind, ExprKind::MethodCall { .. }));
    for source in [
        "actor P { struct A { value int } on fine -> int { 1 } }",
        "actor P { state n = 0; on bad -> int { n. } on fine -> int { 1 } }",
    ] {
        let parsed = parse(source);
        assert!(!parsed.diagnostics.is_empty());
        assert!(
            parsed.program.actors[0]
                .handlers
                .iter()
                .any(|handler| handler.name.text == "fine")
        );
    }
}

#[test]
fn while_blocks_have_bounded_nesting_and_preserve_following_handlers() {
    let parsed = parse(
        "actor P { state n = 0; on work { while n < 3 { let next = n + 1; while n < next { n += 1 } } } on get -> int { n } }",
    );
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert!(matches!(
        parsed.program.actors[0].handlers[0].body.statements[0],
        Statement::While { .. }
    ));
    for body in ["while", "while true", "while true { n = }"] {
        let parsed = parse(&format!(
            "actor P {{ state n = 0; on work {{ {body} }} on get -> int {{ n }} }}"
        ));
        assert!(!parsed.diagnostics.is_empty(), "{body}");
        assert!(
            parsed.program.actors[0]
                .handlers
                .iter()
                .any(|h| h.name.text == "get")
        );
    }
    let nested_63 = format!("{}n += 1{}", "while false { ".repeat(63), " }".repeat(63));
    assert!(
        parse(&format!(
            "actor P {{ state n = 0; on work {{ {nested_63} }} }}"
        ))
        .diagnostics
        .is_empty()
    );
    let nested_64 = format!("{}n += 1{}", "while false { ".repeat(64), " }".repeat(64));
    let parsed = parse(&format!(
        "actor P {{ state n = 0; on work {{ {nested_64} }} on after -> int {{ 1 }} }}"
    ));
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("64-level"))
    );
    assert!(
        parsed.program.actors[0]
            .handlers
            .iter()
            .any(|h| h.name.text == "after")
    );
}

#[test]
fn arithmetic_logic_pipeline_precedence_and_associativity_are_structural() {
    for (text, expected) in [
        ("1 + 2 * 3 - 4 / 2", "((1 + (2 * 3)) - (4 / 2))"),
        ("10 % 3 + 1", "((10 % 3) + 1)"),
        ("9 - 3 - 1", "((9 - 3) - 1)"),
        ("8 / 2 * 3", "((8 / 2) * 3)"),
        ("(1 + 2) * 3", "((1 + 2) * 3)"),
        ("-n * 2", "((-n) * 2)"),
        ("-(1 + 2)", "(-(1 + 2))"),
        ("-12 + 3", "(-12 + 3)"),
        ("!a && b || c", "(((!a) && b) || c)"),
        ("1 + 2 == 3 && 4 < 5", "(((1 + 2) == 3) && (4 < 5))"),
        ("x |> f |> g(2)", "g(f(x), 2)"),
        ("p ! msg(1 + 2)", "(p ! msg((1 + 2)))"),
    ] {
        assert_eq!(shape(&expression(text)), expected, "{text}");
    }
}

#[test]
fn parentheses_and_trailing_operators_allow_multiline_expressions() {
    for text in ["(1\n + 2)", "1 +\n 2", "(1\r\n + 2)", "1 + # continue\n 2"] {
        assert_eq!(shape(&expression(text)), "(1 + 2)", "{text}");
    }
}

#[test]
fn newline_after_complete_expression_starts_a_new_statement() {
    let parsed = parse("actor P { state n = 0\n on get -> int { 1\n +2 } }");
    assert!(!parsed.diagnostics.is_empty());
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message == "expected an expression")
    );
    let parsed = parse("actor P { state n = 0\n on get -> int { 1\n -2 } }");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let body = &parsed.program.actors[0].handlers[0].body;
    assert_eq!(body.statements.len(), 1);
    assert!(matches!(body.statements[0], Statement::Expr(_)));
    assert!(matches!(
        body.tail.as_ref().unwrap().kind,
        ExprKind::Integer(-2)
    ));
}

#[test]
fn semicolon_discards_tail_but_not_bare_tail_expression() {
    for (body, has_tail, statements) in [
        ("", false, 0),
        ("1", true, 0),
        ("1;", false, 1),
        ("let x = 1", false, 1),
        ("n = 2", false, 1),
        ("n += 2", false, 1),
        ("1; 2", true, 1),
    ] {
        let parsed = parse(&format!("actor P {{ state n = 0; on get {{ {body} }} }}"));
        assert!(
            parsed.diagnostics.is_empty(),
            "{body}: {:?}",
            parsed.diagnostics
        );
        let block = &parsed.program.actors[0].handlers[0].body;
        assert_eq!(block.tail.is_some(), has_tail, "{body}");
        assert_eq!(block.statements.len(), statements, "{body}");
    }
}

#[test]
fn missing_statement_separator_is_diagnosed_without_losing_next_handler() {
    let parsed = parse("actor P { state n = 0; on bad { let a = 1 let b = 2; } on good {} }");
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("newline or ';'"))
    );
    assert_eq!(parsed.program.actors[0].handlers[1].name.text, "good");
}

#[test]
fn missing_handler_brace_recovers_at_next_handler() {
    let parsed = parse("actor P { state n = 0; on bad -> int { 1\n on good -> int { n } }");
    assert!(!parsed.diagnostics.is_empty());
    let handlers = &parsed.program.actors[0].handlers;
    assert_eq!(handlers.len(), 2);
    assert_eq!(handlers[1].name.text, "good");
    assert!(matches!(
        handlers[1].body.tail.as_ref().unwrap().kind,
        ExprKind::Name(_)
    ));
}

#[test]
fn missing_actor_brace_recovers_at_next_actor() {
    let parsed = parse("actor First { state n = 0; on get { }\nactor Second { state n = 1 }");
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message == "expected '}' after actor")
    );
    assert_eq!(parsed.program.actors.len(), 2);
    assert_eq!(parsed.program.actors[1].name.text, "Second");
}

#[test]
fn incomplete_parameters_and_eof_produce_bounded_valid_spans() {
    for text in [
        "actor",
        "actor P {",
        "actor P { state n:",
        "actor P { on get(x:",
        "actor P { on get(x: int -> int { x } on next {} }",
        "actor P { state n: int = ; on next {} }",
        "actor P { on get { (1 + } }",
    ] {
        let parsed = parse(text);
        assert!(!parsed.diagnostics.is_empty(), "{text}");
        assert!(parsed.diagnostics.len() <= MAX_DIAGNOSTICS);
        for diagnostic in &parsed.diagnostics {
            assert!(diagnostic.span.start <= diagnostic.span.end);
            assert!(
                text.get(diagnostic.span.start..diagnostic.span.end)
                    .is_some(),
                "{diagnostic:?}"
            );
        }
    }
}

#[test]
fn integer_literals_accept_underscores_i64_boundaries_and_reject_overflow() {
    for (text, expected) in [
        ("9223372036854775807", i64::MAX),
        ("9_223_372_036_854_775_807", i64::MAX),
        ("-9223372036854775808", i64::MIN),
        ("-1_000_000", -1_000_000),
        ("00012", 12),
    ] {
        assert!(matches!(expression(text).kind, ExprKind::Integer(value) if value == expected));
    }
    for text in [
        "9223372036854775808",
        "-9223372036854775809",
        "999999999999999999999999999999999999",
    ] {
        let source = format!("actor P {{ state n: int = {text}; on good {{}} }}");
        let parsed = parse(&source);
        let diagnostic = parsed
            .diagnostics
            .iter()
            .find(|d| d.code == "S004")
            .unwrap();
        assert_eq!(&source[diagnostic.span.start..diagnostic.span.end], text);
        assert!(matches!(
            parsed.program.actors[0].states[0].default.kind,
            ExprKind::Error
        ));
        assert_eq!(parsed.program.actors[0].handlers[0].name.text, "good");
    }
}

#[test]
fn source_size_limit_is_inclusive_and_measured_in_bytes() {
    let at_limit = " ".repeat(MAX_SOURCE_BYTES);
    let parsed = parse(&at_limit);
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.tokens.last().unwrap().kind, TokenKind::Eof);
    let oversized = format!("{at_limit}中");
    let parsed = parse(&oversized);
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].code, "S002");
    assert!(parsed.tokens.is_empty());
    assert!(parsed.program.actors.is_empty());
}

#[test]
fn diagnostic_limit_caps_lexer_and_parser_without_dropping_tokens() {
    let text = "@".repeat(MAX_DIAGNOSTICS * 2);
    let (tokens, diagnostics) = lex(&text);
    assert_eq!(diagnostics.len(), MAX_DIAGNOSTICS);
    assert_eq!(tokens.len(), text.len() + 1);
    let parsed = parse(&text);
    assert_eq!(parsed.diagnostics.len(), MAX_DIAGNOSTICS);
    assert_eq!(parsed.tokens.len(), tokens.len());
}

#[test]
fn expression_work_limit_recovers_and_resets_for_following_handler() {
    let long = std::iter::repeat_n("1", 300)
        .collect::<Vec<_>>()
        .join(" + ");
    let parsed = parse(&format!(
        "actor P {{ state n = 0; on huge -> int {{ {long} }} on next -> int {{ 1 + 2 }} }}"
    ));
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("256-node"))
    );
    let handlers = &parsed.program.actors[0].handlers;
    assert_eq!(handlers.len(), 2);
    assert_eq!(shape(handlers[1].body.tail.as_ref().unwrap()), "(1 + 2)");
}

#[test]
fn trivia_and_empty_input_are_valid_empty_programs() {
    for text in ["", " \r\n\t", "# 注释 😀\n", "// line comment\n"] {
        let parsed = parse(text);
        assert!(parsed.diagnostics.is_empty());
        assert!(parsed.program.actors.is_empty());
        assert_eq!(
            parsed.tokens.last().unwrap().span,
            Span::new(text.len(), text.len())
        );
    }
}
