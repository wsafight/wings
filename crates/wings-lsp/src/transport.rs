use std::io::{self, BufRead, Write};

use serde_json::Value;

const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

pub fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut header_bytes = 0;
    let mut length = None;
    loop {
        let mut line = Vec::new();
        loop {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if header_bytes == 0 {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete LSP header",
                ));
            }
            let count = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |position| position + 1);
            if header_bytes + count > MAX_HEADER_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "LSP header exceeds limit",
                ));
            }
            line.extend_from_slice(&available[..count]);
            header_bytes += count;
            reader.consume(count);
            if line.last() == Some(&b'\n') {
                break;
            }
        }
        let line = std::str::from_utf8(&line).map_err(invalid)?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("invalid LSP header"))?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(invalid("duplicate Content-Length"));
            }
            let parsed = value.trim().parse::<usize>().map_err(invalid)?;
            if parsed > MAX_MESSAGE_BYTES {
                return Err(invalid("LSP message exceeds limit"));
            }
            length = Some(parsed);
        }
    }
    let length = length.ok_or_else(|| invalid("missing Content-Length"))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map(Some).map_err(invalid)
}

pub fn write_message(writer: &mut impl Write, message: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(invalid)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
