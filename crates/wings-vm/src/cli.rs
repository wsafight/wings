use std::fmt;
use std::io::{BufRead, Write};

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use wings_abi::ValueType;
use wings_vm::{CancellationToken, Reservation, ResourceAccount, ResourceLevel, Resources};

const MAX_LINE_BYTES: usize = 64 << 10;
const MAX_NAME_BYTES: usize = 256;
const MAX_ARGUMENTS: usize = 64;
const CONTROL_BYTES: usize = 8 << 10;
const ADAPTER_BYTES: u64 = (4 * MAX_LINE_BYTES + CONTROL_BYTES) as u64;

pub struct Adapter {
    line: Vec<u8>,
    cancellation: CancellationToken,
    _charge: Reservation,
}

pub struct Request {
    pub message: MessageName,
    pub arguments: Arguments,
}

pub struct MessageName {
    bytes: [u8; MAX_NAME_BYTES],
    length: usize,
}

pub struct Arguments {
    values: [Argument; MAX_ARGUMENTS],
    length: usize,
}

#[derive(Clone, Copy)]
enum Argument {
    Int(i64),
    Bool(bool),
    Unit,
    InvalidNumber,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Response {
    Reply { reply: Reply },
    Accepted { accepted: bool },
}

#[derive(Serialize)]
#[serde(untagged)]
enum Reply {
    Int(i64),
    Bool(bool),
    Unit(()),
}

impl Adapter {
    pub fn new(account: &ResourceAccount) -> Result<Self, String> {
        if account.level() != ResourceLevel::Process {
            return Err("CLI adapter requires a process resource account".into());
        }
        let charge = account.reserve(Resources {
            bytes: ADAPTER_BYTES,
            ..Resources::default()
        })?;
        let mut line = Vec::new();
        line.try_reserve_exact(MAX_LINE_BYTES)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            line,
            cancellation: CancellationToken::default(),
            _charge: charge,
        })
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub fn read_request(&mut self, reader: &mut impl BufRead) -> Result<Option<Request>, String> {
        self.line.clear();
        loop {
            let buffer = reader.fill_buf().map_err(|error| error.to_string())?;
            if buffer.is_empty() {
                if self.line.is_empty() {
                    return Ok(None);
                }
                break;
            }
            let length = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(buffer.len(), |index| index + 1);
            if length > MAX_LINE_BYTES - self.line.len() {
                return Err("input line exceeds 64 KiB".into());
            }
            let finished = buffer[length - 1] == b'\n';
            self.line.extend_from_slice(&buffer[..length]);
            reader.consume(length);
            if finished {
                break;
            }
        }
        serde_json::from_slice(&self.line)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    pub fn write_reply(
        &self,
        writer: &mut impl Write,
        reply_type: Option<ValueType>,
        value: Option<i64>,
    ) -> Result<(), String> {
        let response = match value {
            Some(value) => Response::Reply {
                reply: match reply_type {
                    Some(ValueType::Int) => Reply::Int(value),
                    Some(ValueType::Bool) => Reply::Bool(value != 0),
                    _ => Reply::Unit(()),
                },
            },
            None => Response::Accepted { accepted: true },
        };
        serde_json::to_writer(&mut *writer, &response).map_err(|error| error.to_string())?;
        writeln!(writer)
            .and_then(|_| writer.flush())
            .map_err(|error| error.to_string())
    }
}

impl MessageName {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.length]).expect("validated JSON message name")
    }
}

impl Arguments {
    pub fn scalars(
        &self,
        parameters: &[(String, ValueType)],
    ) -> Result<[i64; MAX_ARGUMENTS], String> {
        if self.length != parameters.len() {
            return Err("handler argument count mismatch".into());
        }
        let mut result = [0; MAX_ARGUMENTS];
        for ((slot, value), (_, kind)) in result
            .iter_mut()
            .zip(&self.values[..self.length])
            .zip(parameters)
        {
            *slot = match (kind, value) {
                (ValueType::Int, Argument::Int(value)) => *value,
                (ValueType::Bool, Argument::Bool(value)) => i64::from(*value),
                (ValueType::Unit, Argument::Unit) => 0,
                (ValueType::Int, _) => return Err("expected an i64 argument".into()),
                (ValueType::Bool, _) => return Err("expected a Bool argument".into()),
                (ValueType::Unit, _) => return Err("expected null for Unit".into()),
                _ => return Err("record references are not JSON arguments".into()),
            };
        }
        Ok(result)
    }
}

impl<'de> Deserialize<'de> for MessageName {
    fn deserialize<Parser: Deserializer<'de>>(parser: Parser) -> Result<Self, Parser::Error> {
        struct NameVisitor;
        impl Visitor<'_> for NameVisitor {
            type Value = MessageName;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .write_str("a message name of 1..=256 UTF-8 bytes without control characters")
            }

            fn visit_str<Error: de::Error>(self, value: &str) -> Result<Self::Value, Error> {
                if value.is_empty()
                    || value.len() > MAX_NAME_BYTES
                    || value.chars().any(char::is_control)
                {
                    return Err(Error::custom("invalid message name"));
                }
                let mut name = MessageName {
                    bytes: [0; MAX_NAME_BYTES],
                    length: value.len(),
                };
                name.bytes[..name.length].copy_from_slice(value.as_bytes());
                Ok(name)
            }
        }
        parser.deserialize_str(NameVisitor)
    }
}

impl<'de> Deserialize<'de> for Argument {
    fn deserialize<Parser: Deserializer<'de>>(parser: Parser) -> Result<Self, Parser::Error> {
        struct ArgumentVisitor;
        impl<'de> Visitor<'de> for ArgumentVisitor {
            type Value = Argument;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an Int, Bool or Unit JSON argument")
            }

            fn visit_i64<Error: de::Error>(self, value: i64) -> Result<Self::Value, Error> {
                Ok(Argument::Int(value))
            }

            fn visit_u64<Error: de::Error>(self, value: u64) -> Result<Self::Value, Error> {
                Ok(i64::try_from(value).map_or(Argument::InvalidNumber, Argument::Int))
            }

            fn visit_f64<Error: de::Error>(self, _value: f64) -> Result<Self::Value, Error> {
                Ok(Argument::InvalidNumber)
            }

            fn visit_bool<Error: de::Error>(self, value: bool) -> Result<Self::Value, Error> {
                Ok(Argument::Bool(value))
            }

            fn visit_unit<Error: de::Error>(self) -> Result<Self::Value, Error> {
                Ok(Argument::Unit)
            }

            fn visit_str<Error: de::Error>(self, _value: &str) -> Result<Self::Value, Error> {
                Err(Error::custom("expected an Int, Bool or Unit JSON argument"))
            }
        }
        parser.deserialize_any(ArgumentVisitor)
    }
}

impl<'de> Deserialize<'de> for Arguments {
    fn deserialize<Parser: Deserializer<'de>>(parser: Parser) -> Result<Self, Parser::Error> {
        struct ArgumentsVisitor;
        impl<'de> Visitor<'de> for ArgumentsVisitor {
            type Value = Arguments;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an array of at most 64 scalar arguments")
            }

            fn visit_str<Error: de::Error>(self, _value: &str) -> Result<Self::Value, Error> {
                Err(Error::custom("expected an args array"))
            }

            fn visit_seq<Sequence: SeqAccess<'de>>(
                self,
                mut sequence: Sequence,
            ) -> Result<Self::Value, Sequence::Error> {
                let mut result = Arguments {
                    values: [Argument::Unit; MAX_ARGUMENTS],
                    length: 0,
                };
                while let Some(value) = sequence.next_element()? {
                    if result.length == MAX_ARGUMENTS {
                        return Err(de::Error::custom("args exceeds 64 arguments"));
                    }
                    result.values[result.length] = value;
                    result.length += 1;
                }
                Ok(result)
            }
        }
        parser.deserialize_any(ArgumentsVisitor)
    }
}

enum Field {
    Message,
    Arguments,
}

impl<'de> Deserialize<'de> for Field {
    fn deserialize<Parser: Deserializer<'de>>(parser: Parser) -> Result<Self, Parser::Error> {
        struct FieldVisitor;
        impl Visitor<'_> for FieldVisitor {
            type Value = Field;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("message or args")
            }

            fn visit_str<Error: de::Error>(self, value: &str) -> Result<Self::Value, Error> {
                match value {
                    "message" => Ok(Field::Message),
                    "args" => Ok(Field::Arguments),
                    _ => Err(Error::custom("expected exactly message and args fields")),
                }
            }
        }
        parser.deserialize_identifier(FieldVisitor)
    }
}

impl<'de> Deserialize<'de> for Request {
    fn deserialize<Parser: Deserializer<'de>>(parser: Parser) -> Result<Self, Parser::Error> {
        struct RequestVisitor;
        impl<'de> Visitor<'de> for RequestVisitor {
            type Value = Request;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object with exactly message and args fields")
            }

            fn visit_str<Error: de::Error>(self, _value: &str) -> Result<Self::Value, Error> {
                Err(Error::custom("expected a JSON object"))
            }

            fn visit_map<Fields: MapAccess<'de>>(
                self,
                mut fields: Fields,
            ) -> Result<Self::Value, Fields::Error> {
                let mut message = None;
                let mut arguments = None;
                while let Some(field) = fields.next_key()? {
                    match field {
                        Field::Message if message.is_none() => message = Some(fields.next_value()?),
                        Field::Arguments if arguments.is_none() => {
                            arguments = Some(fields.next_value()?)
                        }
                        _ => return Err(de::Error::custom("duplicate message or args field")),
                    }
                }
                Ok(Request {
                    message: message.ok_or_else(|| de::Error::custom("missing message field"))?,
                    arguments: arguments.ok_or_else(|| de::Error::custom("missing args field"))?,
                })
            }
        }
        parser.deserialize_any(RequestVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, BufReader, Cursor};

    fn limits(bytes: u64) -> Resources {
        Resources {
            bytes,
            ..Resources::default()
        }
    }

    fn accounts() -> (ResourceAccount, ResourceAccount, ResourceAccount) {
        let vm = ResourceAccount::vm_accounted(limits(8 << 20)).unwrap();
        let tenant = vm.tenant(limits(4 << 20)).unwrap();
        let account = tenant.process(limits(2 << 20)).unwrap();
        (vm, tenant, account)
    }

    fn parse(text: &str) -> Request {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn fixed_request_and_response_storage_fit_control_admission() {
        let bytes = 3 * std::mem::size_of::<Request>()
            + std::mem::size_of::<Adapter>()
            + std::mem::size_of::<[i64; MAX_ARGUMENTS]>()
            + std::mem::size_of::<Response>()
            + 64;
        assert!(bytes <= CONTROL_BYTES);
        assert_eq!(ADAPTER_BYTES, 264 << 10);
    }

    #[test]
    fn adapter_admission_checks_each_level_and_exact_budget_without_leaks() {
        let (vm, tenant, account) = accounts();
        let baseline = [account.used(), tenant.used(), vm.used()];
        for invalid in [&vm, &tenant] {
            assert!(Adapter::new(invalid).is_err());
        }
        for owner in [&account, &tenant, &vm] {
            for remaining in [0, ADAPTER_BYTES - 1, ADAPTER_BYTES] {
                let fill = owner
                    .reserve(limits(owner.limit().bytes - owner.used().bytes - remaining))
                    .unwrap();
                let before = [account.used(), tenant.used(), vm.used()];
                if remaining < ADAPTER_BYTES {
                    let error = Adapter::new(&account).err().unwrap();
                    assert!(error.contains(&format!(
                        "resource_exhausted at {:?} account",
                        owner.level()
                    )));
                } else {
                    let adapter = Adapter::new(&account).unwrap();
                    assert_eq!(adapter.line.capacity(), MAX_LINE_BYTES);
                    assert_eq!(adapter._charge.amount(), limits(ADAPTER_BYTES));
                    assert_eq!(owner.used().bytes, owner.limit().bytes);
                    for (used, previous) in [account.used(), tenant.used(), vm.used()]
                        .into_iter()
                        .zip(before)
                    {
                        assert_eq!(used, limits(previous.bytes + ADAPTER_BYTES));
                    }
                    assert!(Adapter::new(&account).is_err());
                    drop(adapter);
                }
                assert_eq!([account.used(), tenant.used(), vm.used()], before);
                drop(fill);
                assert_eq!([account.used(), tenant.used(), vm.used()], baseline);
            }
        }
        drop((account, tenant));
        assert_eq!(
            vm.used(),
            ResourceAccount::vm_accounted(limits(8 << 20))
                .unwrap()
                .used()
        );
    }

    #[test]
    fn requests_accept_escaped_fields_unicode_and_exactly_64_arguments() {
        let request = parse(
            r#"{"args":[-9223372036854775808,9223372036854775807,true,false,null],"mess\u0061ge":"\u0049nc"}"#,
        );
        assert_eq!(request.message.as_str(), "Inc");
        let parameters = [
            ValueType::Int,
            ValueType::Int,
            ValueType::Bool,
            ValueType::Bool,
            ValueType::Unit,
        ]
        .into_iter()
        .map(|kind| (String::new(), kind))
        .collect::<Vec<_>>();
        assert_eq!(
            &request.arguments.scalars(&parameters).unwrap()[..5],
            &[i64::MIN, i64::MAX, 1, 0, 0]
        );
        let unicode = "é🦀";
        assert_eq!(
            parse(&format!(r#"{{"message":"{unicode}","args":[]}}"#))
                .message
                .as_str(),
            unicode
        );
        let text = format!(
            r#"{{"message":"Many","args":[{}]}}"#,
            vec!["0"; 64].join(",")
        );
        let request = parse(&text);
        assert_eq!(request.arguments.length, 64);
        assert!(serde_json::from_str::<Request>(&text.replacen("[0", "[0,0", 1)).is_err());
        for name in ["x".repeat(256), "é".repeat(128)] {
            assert_eq!(
                parse(&format!(r#"{{"message":"{name}","args":[]}}"#))
                    .message
                    .as_str(),
                name
            );
        }
        for name in ["x".repeat(257), "é".repeat(129)] {
            assert!(
                serde_json::from_str::<Request>(&format!(r#"{{"message":"{name}","args":[]}}"#))
                    .is_err()
            );
        }
    }

    #[test]
    fn requests_reject_ambiguous_or_nested_envelopes_without_generic_values() {
        for text in [
            "null",
            "[]",
            "{}",
            "true",
            "0",
            "\"Get\"",
            r#"{"message":"Get"}"#,
            r#"{"args":[]}"#,
            r#"{"message":"Get","args":[],"extra":0}"#,
            r#"{"message":"Missing","message":"Get","args":[]}"#,
            r#"{"message":"Get","args":[99],"args":[]}"#,
            r#"{"message":"Get","mess\u0061ge":"Get","args":[]}"#,
            r#"{"message":"Get","args":null}"#,
            r#"{"message":[],"args":[]}"#,
            r#"{"message":"Get","args":["value"]}"#,
            r#"{"message":"Get","args":[[]]}"#,
            r#"{"message":"Get","args":[{}]}"#,
            r#"{"message":"","args":[]}"#,
            r#"{"message":"Get\n","args":[]}"#,
            r#"{"message":"Get","args":[]} {}"#,
            r#"{"message":"Get","args":[NaN]}"#,
            r#"{"message":"Get","args":[1e400]}"#,
        ] {
            assert!(serde_json::from_str::<Request>(text).is_err(), "{text}");
        }
        for field in ["message", "unknown"] {
            let escaped = "\\u0061".repeat(10_000);
            let text = if field == "message" {
                format!(r#"{{"message":"{escaped}","args":[]}}"#)
            } else {
                format!(r#"{{"{escaped}":0,"message":"Get","args":[]}}"#)
            };
            assert!(text.len() < MAX_LINE_BYTES);
            assert!(serde_json::from_str::<Request>(&text).is_err());
        }
        let deeply_nested = format!(
            r#"{{"message":"Get","args":[{}0{}]}}"#,
            "[".repeat(2000),
            "]".repeat(2000)
        );
        let error = serde_json::from_str::<Request>(&deeply_nested)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("invalid type: sequence"));
        assert!(!error.contains("recursion limit"));
        for text in [
            format!("\"{}\"", "\u{7f}".repeat(60_000)),
            format!(
                r#"{{"message":"Get","args":"{}"}}"#,
                "\u{7f}".repeat(60_000)
            ),
        ] {
            let error = serde_json::from_str::<Request>(&text)
                .err()
                .unwrap()
                .to_string();
            assert!(error.len() < 128);
        }
    }

    #[test]
    fn signature_checks_do_not_coerce_floats_unsigned_overflow_or_handles() {
        for number in [
            "1.0",
            "1e0",
            "-0",
            "9223372036854775808",
            "-9223372036854775809",
        ] {
            let request = parse(&format!(r#"{{"message":"Int","args":[{number}]}}"#));
            assert_eq!(
                request
                    .arguments
                    .scalars(&[(String::new(), ValueType::Int)])
                    .unwrap_err(),
                "expected an i64 argument"
            );
        }
        for (argument, kind, error) in [
            ("true", ValueType::Int, "expected an i64 argument"),
            ("1", ValueType::Bool, "expected a Bool argument"),
            ("false", ValueType::Unit, "expected null for Unit"),
            ("0", ValueType::Ref, "not JSON arguments"),
            ("0", ValueType::ListRef, "not JSON arguments"),
            ("0", ValueType::BytesRef, "not JSON arguments"),
            ("0", ValueType::Pid, "not JSON arguments"),
            ("0", ValueType::RequestId, "not JSON arguments"),
        ] {
            let request = parse(&format!(r#"{{"message":"Typed","args":[{argument}]}}"#));
            assert!(
                request
                    .arguments
                    .scalars(&[(String::new(), kind)])
                    .unwrap_err()
                    .contains(error)
            );
        }
        assert_eq!(
            parse(r#"{"message":"Get","args":[1]}"#)
                .arguments
                .scalars(&[])
                .unwrap_err(),
            "handler argument count mismatch"
        );
    }

    #[test]
    fn exact_line_limit_eof_crlf_and_repeated_errors_reuse_accounted_storage() {
        let (vm, tenant, account) = accounts();
        let baseline = vm.used();
        let mut adapter = Adapter::new(&account).unwrap();
        let charged = vm.used();
        let pointer = adapter.line.as_ptr();
        let request = b"{\"message\":\"Get\",\"args\":[]}";
        let mut text = vec![b' '; MAX_LINE_BYTES];
        text[..request.len()].copy_from_slice(request);
        text[MAX_LINE_BYTES - 1] = b'\n';
        text.extend_from_slice(request);
        text.extend_from_slice(b"\r\n");
        text.extend_from_slice(request);
        let mut reader = BufReader::with_capacity(7, Cursor::new(text));
        for _ in 0..3 {
            assert_eq!(
                adapter
                    .read_request(&mut reader)
                    .unwrap()
                    .unwrap()
                    .message
                    .as_str(),
                "Get"
            );
            assert_eq!(adapter.line.as_ptr(), pointer);
            assert_eq!(adapter.line.capacity(), MAX_LINE_BYTES);
            assert_eq!(vm.used(), charged);
        }
        assert!(adapter.read_request(&mut reader).unwrap().is_none());
        let oversized = vec![b' '; MAX_LINE_BYTES + 1];
        assert!(
            adapter
                .read_request(&mut Cursor::new(oversized))
                .err()
                .unwrap()
                .contains("64 KiB")
        );
        for _ in 0..32 {
            assert!(
                adapter
                    .read_request(&mut Cursor::new(b"{\"message\":\"Get\",\"args\":[[]]}\n"))
                    .is_err()
            );
            assert_eq!(adapter.line.as_ptr(), pointer);
            assert_eq!(vm.used(), charged);
        }
        assert!(
            adapter
                .read_request(&mut Cursor::new(request))
                .unwrap()
                .is_some()
        );
        drop(adapter);
        assert_eq!(vm.used(), baseline);
        drop((account, tenant));
    }

    #[test]
    fn replies_preserve_wire_format_and_write_failures_keep_the_admission() {
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }

            fn flush(&mut self) -> io::Result<()> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        let (vm, tenant, account) = accounts();
        let baseline = vm.used();
        let adapter = Adapter::new(&account).unwrap();
        let mut output = Vec::new();
        for (kind, value, expected) in [
            (
                Some(ValueType::Int),
                Some(i64::MIN),
                "{\"reply\":-9223372036854775808}\n",
            ),
            (
                Some(ValueType::Int),
                Some(i64::MAX),
                "{\"reply\":9223372036854775807}\n",
            ),
            (Some(ValueType::Bool), Some(0), "{\"reply\":false}\n"),
            (Some(ValueType::Bool), Some(1), "{\"reply\":true}\n"),
            (Some(ValueType::Unit), Some(0), "{\"reply\":null}\n"),
            (None, None, "{\"accepted\":true}\n"),
        ] {
            output.clear();
            adapter.write_reply(&mut output, kind, value).unwrap();
            assert_eq!(output, expected.as_bytes());
        }
        assert!(
            adapter
                .write_reply(&mut BrokenWriter, Some(ValueType::Int), Some(7))
                .is_err()
        );
        assert_eq!(vm.used().bytes, baseline.bytes + ADAPTER_BYTES);
        drop(adapter);
        assert_eq!(vm.used(), baseline);
        drop((account, tenant));
    }

    #[test]
    fn input_io_errors_do_not_append_or_release_the_adapter_early() {
        struct BrokenReader;
        impl io::Read for BrokenReader {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        impl BufRead for BrokenReader {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                Err(io::ErrorKind::BrokenPipe.into())
            }

            fn consume(&mut self, _amount: usize) {
                panic!("failed reader must not be consumed");
            }
        }
        let (vm, tenant, account) = accounts();
        let baseline = vm.used();
        let mut adapter = Adapter::new(&account).unwrap();
        let pointer = adapter.line.as_ptr();
        assert!(adapter.read_request(&mut BrokenReader).is_err());
        assert!(adapter.line.is_empty());
        assert_eq!(adapter.line.as_ptr(), pointer);
        assert_eq!(vm.used().bytes, baseline.bytes + ADAPTER_BYTES);
        drop(adapter);
        assert_eq!(vm.used(), baseline);
        drop((account, tenant));
    }
}
