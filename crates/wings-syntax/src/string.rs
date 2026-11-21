use wings_source::Span;

pub(crate) const MAX_STRING_BYTES: usize = 65536;

pub(crate) enum RawSegment<'a> {
    Literal(String),
    Expr {
        slice: &'a str,
        offset: usize,
        span: Span,
    },
}

pub(crate) fn decode(text: &str) -> Result<String, &'static str> {
    let body = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .ok_or("unterminated string literal")?;
    decode_segment(body, false)
}

fn decode_segment(body: &str, collapse_double_braces: bool) -> Result<String, &'static str> {
    let mut decoded = String::new();
    let mut chars = body.chars().peekable();
    while let Some(character) = chars.next() {
        let character = if character == '\\' {
            match chars.next() {
                Some('"') => '"',
                Some('\\') => '\\',
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('0') => '\0',
                Some('$') => '$',
                Some('{') => '{',
                Some('}') => '}',
                _ => return Err("unsupported or incomplete string escape"),
            }
        } else if collapse_double_braces && character == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
                '{'
            } else {
                return Err("unclosed '{' in format string");
            }
        } else if collapse_double_braces && character == '}' {
            if chars.peek() == Some(&'}') {
                chars.next();
                '}'
            } else {
                return Err("unmatched '}' in format string; use '}}' to escape");
            }
        } else if character.is_control() {
            return Err("string control characters must use escapes");
        } else {
            character
        };
        if decoded.len() + character.len_utf8() > MAX_STRING_BYTES {
            return Err("string literal exceeds 65536 UTF-8 bytes");
        }
        decoded.push(character);
    }
    Ok(decoded)
}

pub(crate) fn split_interpolated(
    text: &str,
    token_start: usize,
    is_fstring: bool,
) -> Result<Option<Vec<RawSegment<'_>>>, (&'static str, Span)> {
    let token_span = Span::new(token_start, token_start + text.len());
    let (body, body_offset) = if is_fstring {
        let Some(inner) = text.strip_prefix("f\"").and_then(|s| s.strip_suffix('"')) else {
            return Err(("unterminated format string literal", token_span));
        };
        (inner, token_start + 2)
    } else {
        let Some(inner) = text.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
            return Err(("unterminated string literal", token_span));
        };
        (inner, token_start + 1)
    };

    let bytes = body.as_bytes();
    let mut has_interp = is_fstring;
    if !is_fstring {
        let mut idx = 0;
        let mut esc = false;
        while idx < bytes.len() {
            let b = bytes[idx];
            if esc {
                esc = false;
                idx += 1;
            } else if b == b'\\' {
                esc = true;
                idx += 1;
            } else if b == b'$' && bytes.get(idx + 1) == Some(&b'{') {
                has_interp = true;
                break;
            } else {
                idx += 1;
            }
        }
        if !has_interp {
            return Ok(None);
        }
    }

    let mut segments = Vec::new();
    let mut lit_start = 0usize;
    let mut cursor = 0usize;
    let mut escaped = false;
    while cursor < body.len() {
        let ch = body[cursor..].chars().next().unwrap();
        let step = ch.len_utf8();
        if escaped {
            escaped = false;
            cursor += step;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            cursor += step;
            continue;
        }
        let interp_open_len = if is_fstring && ch == '{' {
            if bytes.get(cursor + 1) == Some(&b'{') {
                cursor += 2;
                continue;
            }
            Some(1usize)
        } else if !is_fstring && ch == '$' && bytes.get(cursor + 1) == Some(&b'{') {
            Some(2usize)
        } else {
            None
        };

        if let Some(open_len) = interp_open_len {
            if cursor > lit_start {
                let decoded = decode_segment(&body[lit_start..cursor], is_fstring)
                    .map_err(|msg| (msg, token_span))?;
                if !decoded.is_empty() {
                    segments.push(RawSegment::Literal(decoded));
                }
            }
            let expr_start = cursor + open_len;
            let mut expr_end = expr_start;
            let mut depth = 1usize;
            let mut in_str = false;
            let mut str_esc = false;
            while expr_end < body.len() && depth > 0 {
                let c = body[expr_end..].chars().next().unwrap();
                let c_len = c.len_utf8();
                if in_str {
                    if str_esc {
                        str_esc = false;
                    } else if c == '\\' {
                        str_esc = true;
                    } else if c == '"' {
                        in_str = false;
                    }
                    expr_end += c_len;
                } else if c == '"' {
                    in_str = true;
                    expr_end += c_len;
                } else if c == '{' {
                    depth += 1;
                    expr_end += c_len;
                } else if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    expr_end += c_len;
                } else {
                    expr_end += c_len;
                }
            }
            if depth != 0 {
                return Err(("unclosed interpolation '{' in string literal", token_span));
            }
            let expr_slice = &body[expr_start..expr_end];
            let abs_start = body_offset + expr_start;
            let abs_end = body_offset + expr_end;
            if expr_slice.trim().is_empty() {
                return Err((
                    "empty interpolation hole in string literal",
                    Span::new(body_offset + cursor, abs_end + 1),
                ));
            }
            segments.push(RawSegment::Expr {
                slice: expr_slice,
                offset: abs_start,
                span: Span::new(abs_start, abs_end),
            });
            cursor = expr_end + 1;
            lit_start = cursor;
            continue;
        }
        cursor += step;
    }

    if lit_start < body.len() {
        let decoded =
            decode_segment(&body[lit_start..], is_fstring).map_err(|msg| (msg, token_span))?;
        if !decoded.is_empty() {
            segments.push(RawSegment::Literal(decoded));
        }
    }

    Ok(Some(segments))
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn decode_bytes(text: &str) -> Result<Vec<u8>, &'static str> {
    let body = text
        .strip_prefix("b\"")
        .and_then(|text| text.strip_suffix('"'))
        .ok_or("unterminated byte literal")?;
    let mut decoded = Vec::new();
    let raw = body.as_bytes();
    let mut idx = 0;
    while idx < raw.len() {
        let byte = raw[idx];
        idx += 1;
        let out = if byte == b'\\' {
            let Some(&esc) = raw.get(idx) else {
                return Err("incomplete byte escape");
            };
            idx += 1;
            match esc {
                b'"' => b'"',
                b'\\' => b'\\',
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'0' => 0,
                b'x' => {
                    let (Some(&hi), Some(&lo)) = (raw.get(idx), raw.get(idx + 1)) else {
                        return Err("incomplete \\xHH byte escape");
                    };
                    let (Some(hi), Some(lo)) = (hex_nibble(hi), hex_nibble(lo)) else {
                        return Err("invalid hex digit in \\xHH byte escape");
                    };
                    idx += 2;
                    (hi << 4) | lo
                }
                _ => return Err("unsupported byte escape"),
            }
        } else if !(0x20..=0x7E).contains(&byte) {
            return Err("byte literals require printable ASCII or \\xHH escapes");
        } else {
            byte
        };
        if decoded.len() == MAX_STRING_BYTES {
            return Err("byte literal exceeds 65536 bytes");
        }
        decoded.push(out);
    }
    Ok(decoded)
}
