use std::io::{self, BufReader, Cursor, Write};

use serde_json::{Value, json};
use wings_lsp::{Server, run};

const URI: &str = "file:///test.wings";
const VALID: &str = "actor P { state n = 0; on get -> int => n }";

#[test]
fn structs_share_state_and_local_hover_and_type_errors() {
    let mut server = server();
    let text = r#"actor P { struct Profile { name: str, count: int } state profile = Profile { name: "中", count: 7 }; on read -> int { let saved = profile; saved.count } }"#;
    assert_eq!(
        open(&mut server, URI, 1, text)[0]["params"]["diagnostics"],
        json!([])
    );
    let offset = text.rfind("saved").unwrap() as u32;
    assert_eq!(
        query(&mut server, "textDocument/hover", URI, 0, offset)["result"]["contents"]["value"],
        "saved: Profile"
    );
    let invalid = text.replace("saved.count", "saved.missing");
    assert!(
        open(&mut server, URI, 2, &invalid)[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "T019")
    );
}

#[test]
fn string_and_list_values_share_hover_completion_and_diagnostics() {
    let mut server = server();
    let text = r#"actor P { state text = "Aé😀"; state xs: [int] = [1, 2]; on read -> int => text.slice(1, 2).len() + xs.head() }"#;
    assert_eq!(
        open(&mut server, URI, 1, text)[0]["params"]["diagnostics"],
        json!([])
    );
    let offset = text.rfind("text").unwrap() as u32;
    let pos = wings_source::SourceFile::new(wings_source::SourceId(0), text)
        .position(offset as usize)
        .unwrap();
    assert_eq!(
        query(
            &mut server,
            "textDocument/hover",
            URI,
            pos.line,
            pos.character
        )["result"]["contents"]["value"],
        "text: str"
    );
    let xs_offset = text.rfind("xs").unwrap() as u32;
    let xs_pos = wings_source::SourceFile::new(wings_source::SourceId(0), text)
        .position(xs_offset as usize)
        .unwrap();
    assert_eq!(
        query(
            &mut server,
            "textDocument/hover",
            URI,
            xs_pos.line,
            xs_pos.character
        )["result"]["contents"]["value"],
        "xs: [int]"
    );

    let completion = query(
        &mut server,
        "textDocument/completion",
        URI,
        pos.line,
        pos.character,
    );
    let items = completion["result"]["items"].as_array().unwrap();
    for label in [
        "actor",
        "struct",
        "match",
        "for",
        "str",
        "[int]",
        "bytes::fill",
        "slice",
        "ask",
        "cancel",
        "gc",
    ] {
        assert!(
            items.iter().any(|item| item["label"] == label),
            "missing completion {label}"
        );
    }

    let invalid = "actor P { on get -> str => b\"raw\" }";
    assert!(
        open(&mut server, URI, 2, invalid)[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "T005")
    );
}

#[test]
fn request_and_reply_handlers_share_definitions_and_hover() {
    for (ty, literal) in [
        ("bool", "false"),
        ("()", "()"),
        ("[int]", "[1, 2]"),
        ("bytes", "b\"a\""),
        ("str", "\"ok\""),
    ] {
        let mut typed_server = server();
        let text = format!(
            "actor P {{ state n = 0; on start {{ let id = self.ask(get_val, timeout: 1_000, then: done); id.cancel() }} on get_val -> {ty} => {literal}; on reply done(id: req, status: int, value: {ty}) {{ let saved = value }} }}"
        );
        assert_eq!(
            open(&mut typed_server, URI, 1, &text)[0]["params"]["diagnostics"],
            json!([])
        );
        let offset = text.rfind("value").unwrap() as u32;
        assert_eq!(
            query(&mut typed_server, "textDocument/hover", URI, 0, offset)["result"]["contents"]["value"],
            format!("value: {ty}")
        );
        let done_ref = text.find("then: done").unwrap() + "then: ".len();
        assert_eq!(
            query(
                &mut typed_server,
                "textDocument/definition",
                URI,
                0,
                done_ref as u32
            )["result"]["range"]["start"]["character"],
            text.rfind("done").unwrap()
        );

        let mut mismatch_server = server();
        let mismatched = text.replace(&format!("value: {ty}"), "value: int");
        let diagnostics = open(&mut mismatch_server, URI, 1, &mismatched);
        assert!(
            diagnostics[0]["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["code"] == "T005")
        );
    }
}

#[test]
fn function_calls_share_signature_definition_and_keyword_completion() {
    let mut server = server();
    let text = "actor P { fn twice(value: int) -> int => value + value; state n = 0; on get -> int => twice(n) }";
    let diagnostics = open(&mut server, URI, 1, text);
    assert!(
        diagnostics[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let offset = text.rfind("twice(n)").unwrap() as u32;
    let hover = query(&mut server, "textDocument/hover", URI, 0, offset);
    assert_eq!(
        hover["result"]["contents"]["value"],
        "twice: (value: int) -> int"
    );
    let definition = query(&mut server, "textDocument/definition", URI, 0, offset);
    assert_eq!(
        definition["result"]["range"]["start"]["character"],
        text.find("twice").unwrap()
    );
}

#[test]
fn initialize_lifecycle_utf16_diagnostics_and_version_ordering_hold() {
    let mut uninit = Server::default();
    let pre = uninit.handle(&request(json!(1), "textDocument/hover", json!({})));
    assert_eq!(pre[0]["error"]["code"], -32002);

    let mut srv = server();
    let source = "actor P {\n  state tag = \"😀\"\n  state bad: int = false\n}";
    let opened = open(&mut srv, URI, 5, source);
    let diags = opened[0]["params"]["diagnostics"].as_array().unwrap();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0]["code"], "T005");
    assert_eq!(diags[0]["range"]["start"]["line"], 2);

    // Stale version ignored
    assert!(
        change(&mut srv, 4, json!([{ "text": VALID }])).is_empty(),
        "stale version should be ignored"
    );
    // Newer version clears diagnostics
    let updated = change(&mut srv, 6, json!([{ "text": VALID }]));
    assert_eq!(updated[0]["params"]["diagnostics"], json!([]));

    // Close clears diagnostics
    let closed = srv.handle(&notification(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": URI } }),
    ));
    assert_eq!(closed[0]["params"]["diagnostics"], json!([]));
}

#[test]
fn clean_shutdown_and_exit_return_true() {
    let (clean, responses) = exchange(&[
        request(json!(1), "initialize", json!({})),
        notification("initialized", json!({})),
        request(json!(2), "shutdown", Value::Null),
        notification("exit", Value::Null),
    ]);
    assert!(clean);
    assert_eq!(responses.len(), 2);
}

#[test]
fn eof_is_not_a_clean_exit_even_after_shutdown() {
    assert!(!exchange(&[]).0);
    assert!(
        !exchange(&[
            request(json!(1), "initialize", json!({})),
            request(json!(2), "shutdown", Value::Null)
        ])
        .0
    );
}

#[test]
fn framing_counts_utf8_bytes_and_handles_one_byte_read_buffers() {
    let message = request(json!("请求😀"), "initialize", json!({"label": "中文😀"}));
    let input = frame(&message);
    let reader = BufReader::with_capacity(1, Cursor::new(input));
    let mut output = Vec::new();
    assert!(!run(reader, &mut output).unwrap());
    let messages = decode_frames(&output);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["id"], "请求😀");
}

#[test]
fn framing_accepts_case_insensitive_length_extra_headers_and_lf() {
    let message = request(json!(1), "initialize", json!({}));
    let body = serde_json::to_vec(&message).unwrap();
    let mut input = format!(
        "content-length: {}\nContent-Type: application/vscode-jsonrpc; charset=utf-8\n\n",
        body.len()
    )
    .into_bytes();
    input.extend(body);
    let mut output = Vec::new();
    assert!(!run(Cursor::new(input), &mut output).unwrap());
    assert_eq!(decode_frames(&output)[0]["id"], 1);
}

#[test]
fn malformed_or_oversized_headers_and_invalid_json_are_rejected() {
    for input in [
        b"\r\n".to_vec(),
        b"Bad header\r\n\r\n".to_vec(),
        b"Content-Length: nope\r\n\r\n".to_vec(),
        b"Content-Length: -1\r\n\r\n".to_vec(),
        b"Content-Length: 2\r\ncontent-length: 2\r\n\r\n{}".to_vec(),
        b"Content-Length: 8388609\r\n\r\n".to_vec(),
        b"Content-Length: 0\r\n\r\n".to_vec(),
        b"Content-Length: 1\r\n\r\n{".to_vec(),
        b"Content-Length: 1\r\n\r\n\xff".to_vec(),
        b"\xff: 1\r\n\r\n".to_vec(),
        vec![b'x'; 8193],
    ] {
        let error = run(Cursor::new(input), Vec::new()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{error}");
    }
}

#[test]
fn truncated_header_and_body_report_unexpected_eof() {
    for input in [
        b"Content-Length: 2".as_slice(),
        b"Content-Length: 2\r\n",
        b"Content-Length: 2\r\n\r\n{",
    ] {
        let error = run(Cursor::new(input), Vec::new()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }
}

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed output"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn transport_propagates_output_errors() {
    let input = frame(&request(json!(1), "initialize", json!({})));
    let error = run(Cursor::new(input), BrokenWriter).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn binary_help_and_bad_arguments_have_distinct_exit_codes() {
    for argument in ["--help", "-h"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_wings-lsp"))
            .arg(argument)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout).unwrap().contains("UTF-16"));
        assert!(output.stderr.is_empty());
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_wings-lsp"))
        .arg("--unknown")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Usage: wings-lsp")
    );
}

fn server() -> Server {
    let mut server = Server::default();
    let responses = server.handle(&request(json!(1), "initialize", json!({})));
    assert_eq!(responses.len(), 1);
    assert!(responses[0].get("result").is_some());
    server.handle(&notification("initialized", json!({})));
    server
}

fn request(id: Value, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

fn open(server: &mut Server, uri: &str, version: i64, text: &str) -> Vec<Value> {
    server.handle(&notification(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "version": version, "text": text } }),
    ))
}

fn change(server: &mut Server, version: i64, changes: Value) -> Vec<Value> {
    server.handle(&notification(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": URI, "version": version }, "contentChanges": changes }),
    ))
}

fn query(server: &mut Server, method: &str, uri: &str, line: u32, character: u32) -> Value {
    let responses = server.handle(&request(
        json!("query"),
        method,
        json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["id"], "query");
    responses.into_iter().next().unwrap()
}

fn frame(message: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(message).unwrap();
    let mut bytes = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    bytes.extend(body);
    bytes
}

fn decode_frames(bytes: &[u8]) -> Vec<Value> {
    let mut messages = Vec::new();
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let split = remaining
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let header = std::str::from_utf8(&remaining[..split]).unwrap();
        let length: usize = header
            .strip_prefix("Content-Length: ")
            .unwrap()
            .parse()
            .unwrap();
        let start = split + 4;
        messages.push(serde_json::from_slice(&remaining[start..start + length]).unwrap());
        remaining = &remaining[start + length..];
    }
    messages
}

fn exchange(messages: &[Value]) -> (bool, Vec<Value>) {
    let input: Vec<u8> = messages.iter().flat_map(frame).collect();
    let mut output = Vec::new();
    let clean = run(Cursor::new(input), &mut output).unwrap();
    (clean, decode_frames(&output))
}
