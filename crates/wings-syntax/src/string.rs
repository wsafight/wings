pub(crate) const MAX_STRING_BYTES: usize = 65536;

pub(crate) fn decode(text: &str) -> Result<String, &'static str> {
    let body = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .ok_or("unterminated string literal")?;
    let mut decoded = String::new();
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        let character = if character == '\\' {
            match characters.next() {
                Some('"') => '"',
                Some('\\') => '\\',
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('0') => '\0',
                _ => return Err("unsupported or incomplete string escape"),
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
