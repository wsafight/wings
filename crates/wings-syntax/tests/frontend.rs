use wings_source::{MAX_DIAGNOSTICS, Span};
use wings_syntax::ast::{BinaryOperator, Expr, ExprKind, Statement};
use wings_syntax::{MAX_SOURCE_BYTES, TokenKind, lex, parse};

fn expression(text: &str) -> Expr {
    let parsed = parse(&format!(
        "process P {{ state n: Int = 0; on Get -> Int {{ {text} }} }}"
    ));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    parsed.program.processes[0].handlers[0]
        .body
        .tail
        .clone()
        .unwrap()
}

fn shape(expression: &Expr) -> String {
    match &expression.kind {
        ExprKind::Integer(value) => value.to_string(),
        ExprKind::Name(name) => name.text.clone(),
        ExprKind::Negate(value) => format!("(-{})", shape(value)),
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
            };
            format!("({} {operator} {})", shape(left), shape(right))
        }
        other => panic!("unexpected expression {other:?}"),
    }
}

#[test]
fn lexer_classifies_keywords_punctuation_and_trivia() {
    let text = "process state on let true false p 42 : , = -> ; { } ( ) + - * / # note\n";
    let (tokens, diagnostics) = lex(text);
    assert!(diagnostics.is_empty());
    let kinds: Vec<_> = tokens
        .iter()
        .filter(|token| !token.kind.is_trivia())
        .map(|token| token.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            TokenKind::Process,
            TokenKind::State,
            TokenKind::On,
            TokenKind::Let,
            TokenKind::True,
            TokenKind::False,
            TokenKind::Identifier,
            TokenKind::Integer,
            TokenKind::Colon,
            TokenKind::Comma,
            TokenKind::Equal,
            TokenKind::Arrow,
            TokenKind::Semicolon,
            TokenKind::LeftBrace,
            TokenKind::RightBrace,
            TokenKind::LeftParen,
            TokenKind::RightParen,
            TokenKind::Plus,
            TokenKind::Minus,
            TokenKind::Star,
            TokenKind::Slash,
            TokenKind::Eof
        ]
    );
    assert!(tokens.iter().any(|token| token.kind == TokenKind::Comment));
    assert!(!TokenKind::Invalid.is_trivia());
}

#[test]
fn lexer_is_lossless_and_spans_use_utf8_bytes() {
    let text = "process 计数器 {\r\n # 😀 e\u{301}\r\n state _值2: Int = 12\n}";
    let (tokens, diagnostics) = lex(text);
    assert!(diagnostics.is_empty());
    let mut cursor = 0;
    let mut reconstructed = String::new();
    for token in &tokens {
        assert_eq!(token.span.start, cursor);
        reconstructed.push_str(&text[token.span.start..token.span.end]);
        cursor = token.span.end;
    }
    assert_eq!(reconstructed, text);
    let name = &tokens[2];
    assert_eq!(name.kind, TokenKind::Identifier);
    assert_eq!(&text[name.span.start..name.span.end], "计数器");
    assert_eq!(name.span.end - name.span.start, 9);
    assert_eq!(
        tokens.last().unwrap().span,
        Span::new(text.len(), text.len())
    );
}

#[test]
fn keywords_require_exact_identifier_matches() {
    let (tokens, diagnostics) = lex("procession State ON let2 true_value _false 名字2");
    assert!(diagnostics.is_empty());
    assert!(
        tokens
            .iter()
            .filter(|token| !token.kind.is_trivia() && token.kind != TokenKind::Eof)
            .all(|token| token.kind == TokenKind::Identifier)
    );
}

#[test]
fn invalid_unicode_characters_report_whole_codepoints_and_keep_lexing() {
    let text = "😀@ ok";
    let (tokens, diagnostics) = lex(text);
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0].code, "S001");
    assert_eq!(diagnostics[0].span, Span::new(0, 4));
    assert_eq!(diagnostics[1].span, Span::new(4, 5));
    assert_eq!(tokens[3].kind, TokenKind::Identifier);
    assert_eq!(&text[tokens[3].span.start..tokens[3].span.end], "ok");
}

#[test]
fn comments_end_at_newline_and_may_end_at_eof() {
    for text in ["# process P {}", "# process P {}\r\n# 尾注释"] {
        let (tokens, diagnostics) = lex(text);
        assert!(diagnostics.is_empty());
        assert!(
            tokens
                .iter()
                .all(|token| token.kind.is_trivia() || token.kind == TokenKind::Eof)
        );
        assert!(parse(text).program.processes.is_empty());
    }
}

#[test]
fn parses_state_parameters_annotations_assignments_and_tail_spans() {
    let text = "process Counter {\n state count: Int = 0\n on Inc(by: Int, enabled: Bool) -> Int {\n let next: Int = count + by\n count = next\n next\n }\n on Reset {}\n on Confirm() -> Unit { () }\n}";
    let parsed = parse(text);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let process = &parsed.program.processes[0];
    assert_eq!(process.name.text, "Counter");
    assert_eq!(process.span, Span::new(0, text.len()));
    assert_eq!(process.states[0].name.text, "count");
    assert_eq!(process.states[0].ty.text, "Int");
    let handler = &process.handlers[0];
    assert_eq!(
        handler
            .parameters
            .iter()
            .map(|p| (p.name.text.as_str(), p.ty.text.as_str()))
            .collect::<Vec<_>>(),
        [("by", "Int"), ("enabled", "Bool")]
    );
    assert_eq!(handler.reply_type.as_ref().unwrap().text, "Int");
    assert!(
        matches!(&handler.body.statements[0], Statement::Let { name, ty: Some(ty), .. } if name.text == "next" && ty.text == "Int")
    );
    assert!(
        matches!(&handler.body.statements[1], Statement::Assign { target, .. } if target.text == "count")
    );
    let tail = handler.body.tail.as_ref().unwrap();
    assert_eq!(&text[tail.span.start..tail.span.end], "next");
    assert!(process.handlers[1].reply_type.is_none());
    assert_eq!(
        process.handlers[2].reply_type.as_ref().unwrap().text,
        "Unit"
    );
    assert!(matches!(
        process.handlers[2].body.tail.as_ref().unwrap().kind,
        ExprKind::Unit
    ));
}

#[test]
fn arithmetic_precedence_associativity_and_negation_are_structural() {
    for (text, expected) in [
        ("1 + 2 * 3 - 4 / 2", "((1 + (2 * 3)) - (4 / 2))"),
        ("9 - 3 - 1", "((9 - 3) - 1)"),
        ("8 / 2 * 3", "((8 / 2) * 3)"),
        ("(1 + 2) * 3", "((1 + 2) * 3)"),
        ("-n * 2", "((-n) * 2)"),
        ("-(1 + 2)", "(-(1 + 2))"),
        ("-12 + 3", "(-12 + 3)"),
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
    let parsed = parse("process P { state n: Int = 0\n on Get -> Int { 1\n +2 } }");
    assert!(!parsed.diagnostics.is_empty());
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message == "expected an expression")
    );
    let parsed = parse("process P { state n: Int = 0\n on Get -> Int { 1\n -2 } }");
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let body = &parsed.program.processes[0].handlers[0].body;
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
        ("1; 2", true, 1),
    ] {
        let parsed = parse(&format!(
            "process P {{ state n: Int = 0; on Get {{ {body} }} }}"
        ));
        assert!(
            parsed.diagnostics.is_empty(),
            "{body}: {:?}",
            parsed.diagnostics
        );
        let block = &parsed.program.processes[0].handlers[0].body;
        assert_eq!(block.tail.is_some(), has_tail, "{body}");
        assert_eq!(block.statements.len(), statements, "{body}");
    }
}

#[test]
fn missing_statement_separator_is_diagnosed_without_losing_next_handler() {
    let parsed =
        parse("process P { state n: Int = 0; on Bad { let a = 1 let b = 2; } on Good {} }");
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("newline or ';'"))
    );
    assert_eq!(parsed.program.processes[0].handlers[1].name.text, "Good");
}

#[test]
fn missing_handler_brace_recovers_at_next_handler() {
    let parsed = parse("process P { state n: Int = 0; on Bad -> Int { 1\n on Good -> Int { n } }");
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message == "expected '}' after handler body")
    );
    let handlers = &parsed.program.processes[0].handlers;
    assert_eq!(handlers.len(), 2);
    assert_eq!(handlers[1].name.text, "Good");
    assert!(matches!(
        handlers[1].body.tail.as_ref().unwrap().kind,
        ExprKind::Name(_)
    ));
}

#[test]
fn missing_process_brace_recovers_at_next_process() {
    let parsed =
        parse("process First { state n: Int = 0; on Get { }\nprocess Second { state n: Int = 1 }");
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message == "expected '}' after process")
    );
    assert_eq!(parsed.program.processes.len(), 2);
    assert_eq!(parsed.program.processes[1].name.text, "Second");
}

#[test]
fn incomplete_parameters_and_eof_produce_bounded_valid_spans() {
    for text in [
        "process",
        "process P {",
        "process P { state n:",
        "process P { on Get(x:",
        "process P { on Get(x: Int -> Int { x } on Next {} }",
        "process P { state n: Int = ; on Next {} }",
        "process P { on Get { (1 + } }",
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
fn integer_literals_accept_i64_boundaries_and_reject_overflow() {
    for (text, expected) in [
        ("9223372036854775807", i64::MAX),
        ("-9223372036854775808", i64::MIN),
        ("00012", 12),
    ] {
        assert!(matches!(expression(text).kind, ExprKind::Integer(value) if value == expected));
    }
    for text in [
        "9223372036854775808",
        "-9223372036854775809",
        "999999999999999999999999999999999999",
    ] {
        let source = format!("process P {{ state n: Int = {text}; on Good {{}} }}");
        let parsed = parse(&source);
        let diagnostic = parsed
            .diagnostics
            .iter()
            .find(|d| d.code == "S004")
            .unwrap();
        assert_eq!(&source[diagnostic.span.start..diagnostic.span.end], text);
        assert!(matches!(
            parsed.program.processes[0].states[0].default.kind,
            ExprKind::Error
        ));
        assert_eq!(parsed.program.processes[0].handlers[0].name.text, "Good");
    }
}

#[test]
fn source_size_limit_is_inclusive_and_measured_in_bytes() {
    let at_limit = " ".repeat(MAX_SOURCE_BYTES);
    let parsed = parse(&at_limit);
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.tokens.last().unwrap().kind, TokenKind::Eof);
    // Exactly one codepoint pushes this source over the byte limit.
    let oversized = format!("{at_limit}中");
    let parsed = parse(&oversized);
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].code, "S002");
    assert!(parsed.tokens.is_empty());
    assert!(parsed.program.processes.is_empty());
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
        "process P {{ state n: Int = 0; on Huge -> Int {{ {long} }} on Next -> Int {{ 1 + 2 }} }}"
    ));
    assert!(
        parsed
            .diagnostics
            .iter()
            .any(|d| d.message.contains("256-node"))
    );
    let handlers = &parsed.program.processes[0].handlers;
    assert_eq!(handlers.len(), 2);
    assert_eq!(shape(handlers[1].body.tail.as_ref().unwrap()), "(1 + 2)");
}

#[test]
fn trivia_and_empty_input_are_valid_empty_programs() {
    for text in ["", " \r\n\t", "# 注释 😀\n"] {
        let parsed = parse(text);
        assert!(parsed.diagnostics.is_empty());
        assert!(parsed.program.processes.is_empty());
        assert_eq!(
            parsed.tokens.last().unwrap().span,
            Span::new(text.len(), text.len())
        );
    }
}
