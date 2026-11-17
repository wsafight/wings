use std::io::Write;

use serde::Serialize;
use wings_abi::{MAX_RECORD_REPLY_NODES, RecordSchema, ValueType};
use wings_vm::{MessageValue, RecordReply};

#[derive(Clone, Copy)]
struct Frame<'value> {
    schema: &'value RecordSchema,
    field: usize,
}

#[cfg(test)]
pub(super) const CONTROL_BYTES: usize = std::mem::size_of::<[Frame<'_>; MAX_RECORD_REPLY_NODES]>()
    + std::mem::size_of::<wings_vm::RecordReplyValues<'_>>()
    + std::mem::size_of::<Leaf<'_>>()
    + 256;

#[derive(Serialize)]
#[serde(untagged)]
enum Leaf<'value> {
    Int(i64),
    Bool(bool),
    Unit(()),
    RecordInt { record_int: i64 },
    IntList { int_list: &'value [i64] },
    Bytes { bytes: &'value [u8] },
    String { string: &'value str },
}

pub fn write_reply(writer: &mut impl Write, reply: &RecordReply) -> Result<(), String> {
    write_record(writer, reply.schema(), reply.schemas(), reply.values())
}

fn write_record<'value>(
    writer: &mut impl Write,
    schema: &'value RecordSchema,
    records: &'value [RecordSchema],
    mut values: impl Iterator<Item = MessageValue<'value>>,
) -> Result<(), String> {
    let mut frames = [Frame { schema, field: 0 }; MAX_RECORD_REPLY_NODES];
    let mut depth = 1;
    let write = |writer: &mut _, bytes: &[u8]| {
        Write::write_all(writer, bytes).map_err(|error| error.to_string())
    };
    write(writer, b"{\"reply\":{")?;
    while depth != 0 {
        let frame = &mut frames[depth - 1];
        let Some((name, kind)) = frame.schema.fields.get(frame.field) else {
            write(writer, b"}")?;
            depth -= 1;
            continue;
        };
        if frame.field != 0 {
            write(writer, b",")?;
        }
        frame.field += 1;
        serde_json::to_writer(&mut *writer, name).map_err(|error| error.to_string())?;
        write(writer, b":")?;
        match kind {
            ValueType::RecordRef(index) => {
                if depth == frames.len() {
                    return Err("record reply exceeds its bounded traversal stack".into());
                }
                let schema = records
                    .get(usize::from(*index))
                    .ok_or("missing record reply schema")?;
                frames[depth] = Frame { schema, field: 0 };
                depth += 1;
                write(writer, b"{")?;
            }
            _ => {
                let value = values.next().ok_or("missing record reply field")?;
                serde_json::to_writer(&mut *writer, &Leaf::from(value))
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    if values.next().is_some() {
        return Err("record reply contains extra leaf values".into());
    }
    write(writer, b"}\n")?;
    writer.flush().map_err(|error| error.to_string())
}

impl<'value> From<MessageValue<'value>> for Leaf<'value> {
    fn from(value: MessageValue<'value>) -> Self {
        match value {
            MessageValue::Int(value) => Self::Int(value),
            MessageValue::Bool(value) => Self::Bool(value),
            MessageValue::Unit => Self::Unit(()),
            MessageValue::RecordInt(record_int) => Self::RecordInt { record_int },
            MessageValue::IntList(int_list) => Self::IntList { int_list },
            MessageValue::Bytes(bytes) => Self::Bytes { bytes },
            MessageValue::String(string) => Self::String { string },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn schema(name: &str, fields: &[(&str, ValueType)]) -> RecordSchema {
        RecordSchema {
            name: name.into(),
            fields: fields
                .iter()
                .map(|(name, kind)| ((*name).into(), *kind))
                .collect(),
        }
    }

    #[test]
    fn nested_records_preserve_field_order_empty_objects_and_leaf_wire_types() {
        let records = [
            schema("Empty", &[]),
            schema(
                "Nested",
                &[
                    ("empty", ValueType::RecordRef(0)),
                    ("record", ValueType::Ref),
                    ("list", ValueType::ListRef),
                    ("bytes", ValueType::BytesRef),
                    ("text", ValueType::StringRef),
                ],
            ),
            schema(
                "Result",
                &[
                    ("min", ValueType::Int),
                    ("max", ValueType::Int),
                    ("nested", ValueType::RecordRef(1)),
                    ("accepted", ValueType::Bool),
                    ("unit", ValueType::Unit),
                ],
            ),
        ];
        let values = [
            MessageValue::Int(i64::MIN),
            MessageValue::Int(i64::MAX),
            MessageValue::RecordInt(-9),
            MessageValue::IntList(&[1, -2]),
            MessageValue::Bytes(&[0, 255]),
            MessageValue::String("中文😀\n\"\\"),
            MessageValue::Bool(false),
            MessageValue::Unit,
        ];
        let mut output = Vec::new();
        write_record(&mut output, &records[2], &records, values.into_iter()).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "{\"reply\":{\"min\":-9223372036854775808,\"max\":9223372036854775807,\"nested\":{\"empty\":{},\"record\":{\"record_int\":-9},\"list\":{\"int_list\":[1,-2]},\"bytes\":{\"bytes\":[0,255]},\"text\":{\"string\":\"中文😀\\n\\\"\\\\\"}},\"accepted\":false,\"unit\":null}}\n"
        );
        let mut output = Vec::new();
        write_record(&mut output, &records[0], &records, [].into_iter()).unwrap();
        assert_eq!(output, b"{\"reply\":{}}\n");
    }

    #[test]
    fn serializer_reports_write_and_flush_failure_without_buffering_a_response() {
        struct Writer {
            remaining: usize,
            flushes: usize,
        }
        impl Write for Writer {
            fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
                if self.remaining == 0 {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                let written = self.remaining.min(buffer.len());
                self.remaining -= written;
                Ok(written)
            }

            fn flush(&mut self) -> io::Result<()> {
                self.flushes += 1;
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        let record = schema("Result", &[("text", ValueType::StringRef)]);
        for remaining in [0, 9, 32, usize::MAX] {
            let mut writer = Writer {
                remaining,
                flushes: 0,
            };
            let result = write_record(
                &mut writer,
                &record,
                &[],
                [MessageValue::String("longer than the short write limit")].into_iter(),
            );
            assert!(result.unwrap_err().contains("broken pipe"));
            assert_eq!(writer.flushes, usize::from(remaining == usize::MAX));
        }
    }
}
