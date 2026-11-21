use std::sync::Arc;

use wings_source::{Diagnostic, Position, SourceFile, SourceId, Span};

fn position(line: u32, character: u32) -> Position {
    Position { line, character }
}

#[test]
fn spans_are_half_open_and_empty_spans_contain_nothing() {
    let span = Span::new(3, 7);
    for (offset, expected) in [(0, false), (2, false), (3, true), (6, true), (7, false)] {
        assert_eq!(span.contains(offset), expected, "offset {offset}");
    }
    assert!(!Span::new(4, 4).contains(4));
    assert_eq!(Span::default(), Span::new(0, 0));
}

#[test]
fn diagnostics_preserve_code_message_and_byte_span() {
    let diagnostic = Diagnostic::error("TEST", String::from("错误"), Span::new(1, 7));
    assert_eq!(diagnostic.code, "TEST");
    assert_eq!(diagnostic.message, "错误");
    assert_eq!(diagnostic.span, Span::new(1, 7));
    assert_eq!(diagnostic.clone(), diagnostic);
}

#[test]
fn empty_source_has_one_empty_line() {
    let source = SourceFile::new(SourceId(42), "");
    assert_eq!(source.id, SourceId(42));
    assert_eq!(source.text(), "");
    assert_eq!(source.position(0), Some(position(0, 0)));
    assert_eq!(source.offset(position(0, 0)), Some(0));
    assert_eq!(source.position(1), None);
    assert_eq!(source.offset(position(0, 1)), None);
    assert_eq!(source.offset(position(1, 0)), None);
}

#[test]
fn utf8_boundaries_round_trip_through_utf16_positions() {
    // Combining marks count separately; astral characters use two UTF-16 units.
    let source = SourceFile::new(SourceId(0), "a中😀e\u{301}\nβ");
    let boundaries = [
        (0, 0, 0),
        (1, 0, 1),
        (4, 0, 2),
        (8, 0, 4),
        (9, 0, 5),
        (11, 0, 6),
        (12, 1, 0),
        (14, 1, 1),
    ];
    for (offset, line, character) in boundaries {
        let expected = position(line, character);
        assert_eq!(source.position(offset), Some(expected), "offset {offset}");
        assert_eq!(
            source.offset(expected),
            Some(offset),
            "position {expected:?}"
        );
    }
}

#[test]
fn rejects_mid_codepoint_mid_surrogate_and_out_of_bounds_positions() {
    let source = SourceFile::new(SourceId(0), "中😀");
    for offset in [1, 2, 4, 5, 6, 8, usize::MAX] {
        assert_eq!(source.position(offset), None, "offset {offset}");
    }
    for invalid in [
        position(0, 2),
        position(0, 4),
        position(1, 0),
        position(u32::MAX, u32::MAX),
    ] {
        assert_eq!(source.offset(invalid), None, "position {invalid:?}");
    }
}

#[test]
fn crlf_offsets_clamp_to_line_end_and_next_line_starts_after_lf() {
    let source = SourceFile::new(SourceId(0), "中\r\n😀\r\n");
    for offset in [3, 4] {
        assert_eq!(source.position(offset), Some(position(0, 1)));
    }
    for offset in [9, 10] {
        assert_eq!(source.position(offset), Some(position(1, 2)));
    }
    assert_eq!(source.offset(position(0, 1)), Some(3));
    assert_eq!(source.offset(position(1, 2)), Some(9));
    assert_eq!(source.position(5), Some(position(1, 0)));
    assert_eq!(source.position(11), Some(position(2, 0)));
    assert_eq!(source.offset(position(2, 0)), Some(11));
    assert_eq!(source.offset(position(0, 2)), None);
}

#[test]
fn adjacent_newlines_and_trailing_newline_keep_empty_lines() {
    let source = SourceFile::new(SourceId(0), "\n\r\nx\n");
    for (offset, expected) in [
        (0, position(0, 0)),
        (1, position(1, 0)),
        (2, position(1, 0)),
        (3, position(2, 0)),
        (4, position(2, 1)),
        (5, position(3, 0)),
    ] {
        assert_eq!(source.position(offset), Some(expected));
    }
    for (line, offset) in [(0, 0), (1, 1), (2, 3), (3, 5)] {
        assert_eq!(source.offset(position(line, 0)), Some(offset));
    }
    assert_eq!(source.offset(position(4, 0)), None);
}

#[test]
fn cloned_sources_preserve_owned_text_and_line_index() {
    let text: Arc<str> = Arc::from("first\n中文");
    let original = SourceFile::new(SourceId(9), text.clone());
    let cloned = original.clone();
    drop(original);
    drop(text);
    assert_eq!(cloned.id, SourceId(9));
    assert_eq!(cloned.text(), "first\n中文");
    assert_eq!(cloned.offset(position(1, 2)), Some(12));
    assert_eq!(cloned.position(12), Some(position(1, 2)));
}
