use std::sync::Arc;

pub const MAX_DIAGNOSTICS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceId(pub u32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn contains(self, offset: usize) -> bool {
        self.start <= offset && offset < self.end
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: impl Into<String>, span: Span) -> Self {
        Self {
            code,
            message: message.into(),
            span,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Clone, Debug)]
pub struct SourceFile {
    pub id: SourceId,
    text: Arc<str>,
    line_starts: Vec<usize>,
}

impl SourceFile {
    pub fn new(id: SourceId, text: impl Into<Arc<str>>) -> Self {
        let text = text.into();
        let mut line_starts = vec![0];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self {
            id,
            text,
            line_starts,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    fn line_end(&self, line: usize) -> usize {
        let mut end = self
            .line_starts
            .get(line + 1)
            .map_or(self.text.len(), |next| next - 1);
        if self.text.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r') {
            end -= 1;
        }
        end
    }

    pub fn position(&self, offset: usize) -> Option<Position> {
        if !self.text.is_char_boundary(offset) {
            return None;
        }
        let line = self.line_starts.partition_point(|start| *start <= offset) - 1;
        let end = offset.min(self.line_end(line));
        let character = self.text[self.line_starts[line]..end]
            .encode_utf16()
            .count();
        Some(Position {
            line: u32::try_from(line).ok()?,
            character: u32::try_from(character).ok()?,
        })
    }

    pub fn offset(&self, position: Position) -> Option<usize> {
        let line = usize::try_from(position.line).ok()?;
        let start = *self.line_starts.get(line)?;
        let end = self.line_end(line);
        let mut units = 0;
        for (relative, character) in self.text[start..end].char_indices() {
            if units == position.character {
                return Some(start + relative);
            }
            units += character.len_utf16() as u32;
            if units > position.character {
                return None;
            }
        }
        (units == position.character).then_some(end)
    }
}
