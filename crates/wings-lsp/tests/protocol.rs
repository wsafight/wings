use std::io::{self, BufReader, Cursor, Write};

use serde_json::{Value, json};
use wings_lsp::{Server, run};

const URI: &str = "file:///test.wings";
const VALID: &str = "process P { state n: Int = 0; on Get -> Int { n } }";

#[test]
fn string_values_share_hover_completion_and_external_type_diagnostics() {
    let mut server = server();
    let text = "process P { state text: String = string_from_bytes(fill_bytes(0, 0)); on Read -> Int { string_len(text) } }";
    assert_eq!(
        open(&mut server, URI, 1, text)[0]["params"]["diagnostics"],
        json!([])
    );
    let offset = text.rfind("text").unwrap() as u32;
    assert_eq!(
        query(&mut server, "textDocument/hover", URI, 0, offset)["result"]["contents"]["value"],
        "text: String"
    );
    let completion = query(&mut server, "textDocument/completion", URI, 0, offset);
    let items = completion["result"]["items"].as_array().unwrap();
    for (name, detail) in [
        ("string_from_bytes", "(Bytes) -> String"),
        ("string_bytes", "(String) -> Bytes"),
        ("string_len", "(String) -> Int"),
        ("concat_strings", "(String, String) -> String"),
    ] {
        assert!(
            items
                .iter()
                .any(|item| item["label"] == name && item["detail"] == detail)
        );
    }
    assert!(items.iter().any(|item| item["label"] == "String"));
    let messages =
        "process P { state n: Int = 0; on Set(value: String) { n = string_len(value) } }";
    assert_eq!(
        open(&mut server, URI, 2, messages)[0]["params"]["diagnostics"],
        json!([])
    );
    let replies = "process P { state n: Int = 0; on Get -> String { \"\" } }";
    assert_eq!(
        open(&mut server, URI, 3, replies)[0]["params"]["diagnostics"],
        json!([])
    );
    let invalid = "process P { state n: Int = 0; on Get -> String { fill_bytes(0, 0) } }";
    assert!(
        open(&mut server, URI, 4, invalid)[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "T005")
    );
}

#[test]
fn string_literal_diagnostics_and_inferred_hover_share_the_parser() {
    let mut server = server();
    let source = r#"process P { state text: String = "中文😀"; on Read -> Int { let saved = "\"\n\0"; string_len(saved) } }"#;
    assert_eq!(
        open(&mut server, URI, 1, source)[0]["params"]["diagnostics"],
        json!([])
    );
    let offset = source.rfind("saved").unwrap() as u32;
    let position = wings_source::SourceFile::new(wings_source::SourceId(0), source)
        .position(offset as usize)
        .unwrap();
    assert_eq!(
        query(
            &mut server,
            "textDocument/hover",
            URI,
            position.line,
            position.character
        )["result"]["contents"]["value"],
        "saved: String"
    );
    let invalid = source.replace("中文😀", r"\q");
    assert!(
        open(&mut server, URI, 2, &invalid)[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "S005")
    );
}

#[test]
fn bytes_hover_and_builtins_share_frontend_types_without_execution() {
    let mut server = server();
    let text =
        "process P { state data: Bytes = fill_bytes(4, 0); on Read -> Int { bytes_len(data) } }";
    assert!(
        open(&mut server, URI, 1, text)[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let offset = text.rfind("data").unwrap();
    assert_eq!(
        query(&mut server, "textDocument/hover", URI, 0, offset as u32)["result"]["contents"]["value"],
        "data: Bytes"
    );
    let completion = query(
        &mut server,
        "textDocument/completion",
        URI,
        0,
        offset as u32,
    );
    for label in [
        "Bytes",
        "fill_bytes",
        "bytes_len",
        "byte_at",
        "replace_byte",
        "read_i64_be",
        "replace_i64_be",
    ] {
        assert!(
            completion["result"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == label)
        );
    }
}

fn request(id: Value, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

fn initialize(server: &mut Server) -> Value {
    let responses = server.handle(&request(json!(1), "initialize", json!({})));
    assert_eq!(responses.len(), 1);
    assert!(responses[0].get("error").is_none(), "{responses:?}");
    responses.into_iter().next().unwrap()
}

fn server() -> Server {
    let mut server = Server::default();
    initialize(&mut server);
    server
}

#[test]
fn while_local_queries_do_not_leak_body_scope() {
    let mut server = server();
    let text = "process P { state n: Int = 0; on Count(limit: Int) -> Int { while n < limit { let inside = n + 1; n = inside }; n } }";
    let diagnostics = open(&mut server, URI, 1, text);
    assert!(
        diagnostics[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let inside = text.rfind("inside").unwrap() as u32;
    let hover = query(&mut server, "textDocument/hover", URI, 0, inside);
    assert_eq!(hover["result"]["contents"]["value"], "inside: Int");
    let outside = text.rfind("n }").unwrap() as u32;
    let completion = query(&mut server, "textDocument/completion", URI, 0, outside);
    let items = completion["result"]["items"].as_array().unwrap();
    assert!(!items.iter().any(|item| item["label"] == "inside"));
    assert!(items.iter().any(|item| item["label"] == "while"));
}

#[test]
fn conditional_queries_share_scope_and_keyword_completion() {
    let mut server = server();
    let text =
        "process P { state n: Int = 0; on Get(flag: Bool) -> Int { if flag { n } else { 7 } } }";
    let diagnostics = open(&mut server, URI, 1, text);
    assert!(
        diagnostics[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let offset = text.find("if flag").unwrap() as u32 + 3;
    let hover = query(&mut server, "textDocument/hover", URI, 0, offset);
    assert_eq!(hover["result"]["contents"]["value"], "flag: Bool");
    let completion = query(&mut server, "textDocument/completion", URI, 0, offset);
    let items = completion["result"]["items"].as_array().unwrap();
    for label in ["if", "else"] {
        assert!(items.iter().any(|item| item["label"] == label));
    }
}

#[test]
fn self_send_targets_resolve_to_forward_handler_definitions() {
    let mut server = server();
    let text = "process P { state n: Int = 0; on Start { send_self(Add, 1) } on Add(value: Int) { n = value } }";
    assert_eq!(
        open(&mut server, URI, 1, text)[0]["params"]["diagnostics"],
        json!([])
    );
    let offset = text.find("Add, 1").unwrap() as u32;
    let hover = query(&mut server, "textDocument/hover", URI, 0, offset);
    assert_eq!(
        hover["result"]["contents"]["value"],
        "Add: (value: Int) (one-way)"
    );
    let definition = query(&mut server, "textDocument/definition", URI, 0, offset);
    assert_eq!(
        definition["result"]["range"]["start"]["character"],
        text.rfind("Add").unwrap()
    );
    let completion = query(&mut server, "textDocument/completion", URI, 0, offset);
    assert!(
        completion["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["label"] == "send_self")
    );
}

#[test]
fn process_capabilities_share_local_types_and_send_target_navigation() {
    let mut server = server();
    let text = "process P { state n: Int = 0; on Start { let child = spawn_self(); send(child, Add, 1) } on Add(value: Int) { n = value } }";
    assert_eq!(
        open(&mut server, URI, 1, text)[0]["params"]["diagnostics"],
        json!([])
    );
    let offset = text.find("child, Add").unwrap() as u32;
    assert_eq!(
        query(&mut server, "textDocument/hover", URI, 0, offset)["result"]["contents"]["value"],
        "child: Pid"
    );
    let offset = text.find("Add, 1").unwrap() as u32;
    assert_eq!(
        query(&mut server, "textDocument/definition", URI, 0, offset)["result"]["range"]["start"]["character"],
        text.rfind("Add").unwrap()
    );
    let completion = query(&mut server, "textDocument/completion", URI, 0, offset);
    for label in ["Pid", "spawn_self", "self_pid", "send"] {
        assert!(
            completion["result"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == label)
        );
    }
}

#[test]
fn request_ids_and_completion_names_use_shared_analysis() {
    let mut server = server();
    let text = "process P { state count: Int = 0; on Start { let id = request(self_pid(), Get, Done, 1000); cancel_request(id) } on Get -> Int { count } on Done(id: RequestId, status: Int, value: Int) { count = value } }";
    assert_eq!(
        open(&mut server, URI, 1, text)[0]["params"]["diagnostics"],
        json!([])
    );
    let offset = text.find("id)").unwrap() as u32;
    assert_eq!(
        query(&mut server, "textDocument/hover", URI, 0, offset)["result"]["contents"]["value"],
        "id: RequestId"
    );
    let offset = text.find("Done, 1000").unwrap() as u32;
    assert_eq!(
        query(&mut server, "textDocument/definition", URI, 0, offset)["result"]["range"]["start"]["character"],
        text.rfind("Done").unwrap()
    );
    let completion = query(&mut server, "textDocument/completion", URI, 0, offset);
    for label in ["RequestId", "request", "cancel_request"] {
        assert!(
            completion["result"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == label)
        );
    }
}

#[test]
fn copied_completion_hover_and_diagnostics_share_request_reply_types() {
    for (ty, literal) in [
        ("Bool", "false"),
        ("Unit", "()"),
        ("RecordInt", "record_int(7)"),
        ("IntList", "empty_ints()"),
        ("Bytes", "fill_bytes(1, 7)"),
    ] {
        let mut typed_server = server();
        let text = format!(
            "process P {{ state n: Int = 0; on Start {{ let id = request(self_pid(), Get, Done, 1000); cancel_request(id) }} on Get -> {ty} {{ {literal} }} on Done(id: RequestId, status: Int, value: {ty}) {{ let saved = value }} }}"
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
        let mut server = server();
        let mismatched = text.replace(&format!("value: {ty}"), "value: Int");
        let diagnostics = open(&mut server, URI, 1, &mismatched);
        assert!(
            diagnostics[0]["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["code"] == "T018")
        );
    }
}

#[test]
fn function_calls_share_signature_definition_and_keyword_completion() {
    let mut server = server();
    let text = "process P { fn twice(value: Int) -> Int { value + value } state n: Int = 0; on Get -> Int { twice(n) } }";
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
        "twice: (value: Int) -> Int"
    );
    let definition = query(&mut server, "textDocument/definition", URI, 0, offset);
    assert_eq!(
        definition["result"]["range"]["start"]["character"],
        text.find("twice").unwrap()
    );
    let completion = query(&mut server, "textDocument/completion", URI, 0, offset);
    for label in ["fn", "twice"] {
        assert!(
            completion["result"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == label)
        );
    }
}

#[test]
fn list_values_share_types_hover_and_builtin_completion() {
    let mut server = server();
    let text = "process P { state values: IntList = empty_ints(); on Push(value: Int) -> Int { values = prepend_int(value, values); head_int(values) } }";
    let diagnostics = open(&mut server, URI, 1, text);
    assert!(
        diagnostics[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let offset = text.rfind("values").unwrap() as u32;
    assert_eq!(
        query(&mut server, "textDocument/hover", URI, 0, offset)["result"]["contents"]["value"],
        "values: IntList"
    );
    let completion = query(&mut server, "textDocument/completion", URI, 0, offset);
    for label in [
        "IntList",
        "empty_ints",
        "prepend_int",
        "head_int",
        "tail_ints",
        "is_empty_ints",
    ] {
        assert!(
            completion["result"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == label)
        );
    }
}

#[test]
fn record_state_shares_types_definitions_and_diagnostics() {
    let mut server = server();
    let text = "process P { state saved: RecordInt = if true { record_int(7) } else { record_int(9) }; on Get -> Int { while read_int(saved) < 11 { saved = if true { record_int(11) } else { saved } }; collect(); read_int(saved) } }";
    let diagnostics = open(&mut server, URI, 1, text);
    assert!(
        diagnostics[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let offset = text.rfind("saved").unwrap() as u32;
    let hover = query(&mut server, "textDocument/hover", URI, 0, offset);
    assert_eq!(hover["result"]["contents"]["value"], "saved: RecordInt");
    let definition = query(&mut server, "textDocument/definition", URI, 0, offset);
    assert_eq!(
        definition["result"]["range"]["start"]["character"],
        text.find("saved").unwrap()
    );
}

#[test]
fn record_locals_share_types_definitions_and_builtin_completion() {
    let mut server = server();
    let text = "process P { state n: Int = 0; on Get -> Int { let saved = record_int(7); read_int(saved) } }";
    let diagnostics = open(&mut server, URI, 1, text);
    assert!(
        diagnostics[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let offset = text.rfind("saved").unwrap() as u32;
    let hover = query(&mut server, "textDocument/hover", URI, 0, offset);
    assert_eq!(hover["result"]["contents"]["value"], "saved: RecordInt");
    let definition = query(&mut server, "textDocument/definition", URI, 0, offset);
    assert_eq!(
        definition["result"]["range"]["start"]["character"],
        text.find("saved").unwrap()
    );
    let completion = query(&mut server, "textDocument/completion", URI, 0, offset);
    let items = completion["result"]["items"].as_array().unwrap();
    for label in ["RecordInt", "record_int", "read_int", "collect", "saved"] {
        assert!(items.iter().any(|item| item["label"] == label));
    }
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
    let responses = server.handle(&request(json!("query"), method,
        json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } })));
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
    let input: Vec<_> = messages.iter().flat_map(frame).collect();
    let mut output = Vec::new();
    let clean = run(Cursor::new(input), &mut output).unwrap();
    (clean, decode_frames(&output))
}

#[test]
fn initialize_advertises_only_implemented_utf16_full_sync_features() {
    let response = initialize(&mut Server::default());
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(
        response["result"]["capabilities"],
        json!({
            "positionEncoding": "utf-16", "textDocumentSync": { "openClose": true, "change": 1 },
            "hoverProvider": true, "definitionProvider": true,
            "completionProvider": { "resolveProvider": false }
        })
    );
    assert_eq!(response["result"]["serverInfo"]["name"], "wings-lsp");
    assert_eq!(
        response["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn encoding_negotiation_rejects_unsupported_encoding_and_allows_retry() {
    let mut server = Server::default();
    let response = server.handle(&request(
        json!(1),
        "initialize",
        json!({ "capabilities": { "general": { "positionEncodings": ["utf-8", "utf-32"] } } }),
    ));
    assert_eq!(response[0]["error"]["code"], -32602);
    let response = server.handle(&request(
        json!(2),
        "initialize",
        json!({ "capabilities": { "general": { "positionEncodings": ["utf-8", "utf-16"] } } }),
    ));
    assert_eq!(
        response[0]["result"]["capabilities"]["positionEncoding"],
        "utf-16"
    );
}

#[test]
fn requests_before_initialization_fail_and_notifications_do_not_initialize() {
    let mut server = Server::default();
    assert!(
        server
            .handle(&notification("initialize", json!({})))
            .is_empty()
    );
    assert!(open(&mut server, URI, 1, VALID).is_empty());
    let response = server.handle(&request(json!(7), "textDocument/hover", json!({})));
    assert_eq!(response[0]["id"], 7);
    assert_eq!(response[0]["error"]["code"], -32002);
    initialize(&mut server);
    assert_eq!(
        query(&mut server, "textDocument/hover", URI, 0, 0)["error"]["code"],
        -32602
    );
}

#[test]
fn invalid_jsonrpc_envelopes_are_rejected_without_panicking() {
    let mut server = server();
    for message in [
        json!(null),
        json!([]),
        json!({"method": "initialize"}),
        json!({"jsonrpc": "1.0", "id": 1, "method": "initialize"}),
        json!({"jsonrpc": "2.0", "id": true, "method": "initialize"}),
        json!({"jsonrpc": "2.0", "id": [], "method": "initialize"}),
    ] {
        let response = server.handle(&message);
        assert_eq!(response.len(), 1);
        assert_eq!(response[0]["id"], Value::Null);
        assert_eq!(response[0]["error"]["code"], -32600);
    }
}

#[test]
fn unknown_methods_return_method_not_found_only_for_requests() {
    let mut server = server();
    let response = server.handle(&request(
        json!("opaque-id"),
        "workspace/notSupported",
        json!({}),
    ));
    assert_eq!(response[0]["id"], "opaque-id");
    assert_eq!(response[0]["error"]["code"], -32601);
    assert!(
        server
            .handle(&notification("workspace/notSupported", json!({})))
            .is_empty()
    );
    for method in ["initialized", "$/cancelRequest", "$/setTrace"] {
        assert!(server.handle(&notification(method, json!({}))).is_empty());
    }
}

#[test]
fn shutdown_requires_request_and_rejects_subsequent_requests() {
    let mut server = server();
    assert!(
        server
            .handle(&notification("shutdown", json!({})))
            .is_empty()
    );
    assert_eq!(open(&mut server, URI, 1, VALID).len(), 1);
    let response = server.handle(&request(json!(2), "shutdown", Value::Null));
    assert_eq!(
        response,
        [json!({"jsonrpc": "2.0", "id": 2, "result": null})]
    );
    let response = server.handle(&request(json!(3), "initialize", json!({})));
    assert_eq!(response[0]["error"]["code"], -32600);
    assert!(open(&mut server, URI, 2, VALID).is_empty());
}

#[test]
fn opening_document_publishes_shared_frontend_diagnostics_and_version() {
    let mut server = server();
    let messages = open(&mut server, URI, 4, "process P { state n: Int = false }");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["method"], "textDocument/publishDiagnostics");
    assert_eq!(messages[0]["params"]["uri"], URI);
    assert_eq!(messages[0]["params"]["version"], 4);
    let diagnostic = &messages[0]["params"]["diagnostics"][0];
    assert_eq!(diagnostic["code"], "T005");
    assert_eq!(diagnostic["source"], "wings");
    assert_eq!(diagnostic["severity"], 1);
    assert_eq!(
        diagnostic["range"],
        json!({"start": {"line": 0, "character": 27}, "end": {"line": 0, "character": 32}})
    );
    let messages = change(&mut server, 5, json!([{"text": VALID}]));
    assert_eq!(messages[0]["params"]["diagnostics"], json!([]));
    assert_eq!(messages[0]["params"]["version"], 5);
}

#[test]
fn diagnostic_ranges_count_astral_characters_as_two_utf16_units() {
    let mut server = server();
    let text = "process 计数器 { state 数值: Int = 0; on Get { 😀 } }";
    let messages = open(&mut server, URI, 1, text);
    let diagnostics = messages[0]["params"]["diagnostics"].as_array().unwrap();
    let diagnostic = diagnostics.iter().find(|d| d["code"] == "S001").unwrap();
    let start = text[..text.find('😀').unwrap()].encode_utf16().count();
    assert_eq!(diagnostic["range"]["start"]["character"], start);
    assert_eq!(diagnostic["range"]["end"]["character"], start + 2);
}

#[test]
fn hover_and_definition_map_unicode_and_crlf_positions() {
    let mut server = server();
    let text = "# 😀\r\nprocess 计数器 {\r\n state 数值: Int = 0\r\n on Get(增量: Int) -> Int { 数值 + 增量 }\r\n}";
    assert_eq!(
        open(&mut server, URI, 1, text)[0]["params"]["diagnostics"],
        json!([])
    );
    let hover = query(&mut server, "textDocument/hover", URI, 3, 26);
    assert_eq!(
        hover["result"]["contents"],
        json!({"kind": "plaintext", "value": "数值: Int"})
    );
    let definition = query(&mut server, "textDocument/definition", URI, 3, 27);
    assert_eq!(
        definition["result"],
        json!({"uri": URI, "range": {"start": {"line": 2, "character": 7}, "end": {"line": 2, "character": 9}}})
    );
    let parameter = query(&mut server, "textDocument/hover", URI, 3, 31);
    assert_eq!(parameter["result"]["contents"]["value"], "增量: Int");
}

#[test]
fn hovering_message_declarations_preserves_one_way_vs_unit_reply() {
    let mut server = server();
    let text = "process P { state n: Int = 0; on Reset {} on Confirm -> Unit {} }";
    open(&mut server, URI, 1, text);
    let response = query(
        &mut server,
        "textDocument/hover",
        URI,
        0,
        text.find("Reset").unwrap() as u32,
    );
    assert_eq!(
        response["result"]["contents"]["value"],
        "Reset: () (one-way)"
    );
    let response = query(
        &mut server,
        "textDocument/hover",
        URI,
        0,
        text.find("Confirm").unwrap() as u32,
    );
    assert_eq!(
        response["result"]["contents"]["value"],
        "Confirm: () -> Unit"
    );
}

#[test]
fn queries_on_whitespace_return_null_but_invalid_positions_are_errors() {
    let mut server = server();
    open(&mut server, URI, 1, "# 😀\nprocess P { state n: Int = 0 }");
    for method in ["textDocument/hover", "textDocument/definition"] {
        assert_eq!(query(&mut server, method, URI, 0, 0)["result"], Value::Null);
        for (line, character) in [(0, 3), (0, 5), (2, 0), (u32::MAX, 0)] {
            assert_eq!(
                query(&mut server, method, URI, line, character)["error"]["code"],
                -32602
            );
        }
        assert_eq!(
            query(&mut server, method, "file:///missing.wings", 0, 0)["error"]["code"],
            -32602
        );
    }
}

#[test]
fn malformed_query_parameters_do_not_panic_or_truncate_coordinates() {
    let mut server = server();
    open(&mut server, URI, 1, VALID);
    for position in [
        json!(null),
        json!({"line": -1, "character": 0}),
        json!({"line": 0, "character": "0"}),
        json!({"line": 0}),
        json!({"line": 0, "character": u64::from(u32::MAX) + 1}),
        json!({"line": u64::from(u32::MAX) + 1, "character": 0}),
    ] {
        let response = server.handle(&request(
            json!(1),
            "textDocument/hover",
            json!({"textDocument": {"uri": URI}, "position": position}),
        ));
        assert_eq!(response[0]["error"]["code"], -32602);
    }
    assert!(
        server
            .handle(&notification("textDocument/hover", json!({})))
            .is_empty()
    );
}

#[test]
fn completion_is_sorted_unique_and_obeys_local_visibility() {
    let mut server = server();
    let text = "process P { state n: Int = 0; on Get(value: Int) -> Int { let first = value; let second = first; second } }";
    open(&mut server, URI, 1, text);
    let before = query(
        &mut server,
        "textDocument/completion",
        URI,
        0,
        text.find("let first").unwrap() as u32,
    );
    let after = query(
        &mut server,
        "textDocument/completion",
        URI,
        0,
        text.rfind("second").unwrap() as u32,
    );
    assert_eq!(after["result"]["isIncomplete"], false);
    let labels = |response: &Value| {
        response["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["label"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let before_labels = labels(&before);
    assert!(!before_labels.contains(&"first".into()));
    assert!(!before_labels.contains(&"second".into()));
    let after_labels = labels(&after);
    assert!(after_labels.windows(2).all(|pair| pair[0] < pair[1]));
    for expected in [
        "process", "state", "on", "let", "true", "false", "Int", "Bool", "Unit", "P", "Get", "n",
        "value", "first", "second",
    ] {
        assert!(
            after_labels.iter().any(|label| label == expected),
            "missing {expected}"
        );
    }
    for (label, kind) in [
        ("process", 14),
        ("Int", 7),
        ("P", 7),
        ("Get", 3),
        ("first", 6),
    ] {
        let item = after["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["label"] == label)
            .unwrap();
        assert_eq!(item["kind"], kind);
    }
}

#[test]
fn stale_and_duplicate_versions_do_not_replace_newer_document() {
    let mut server = server();
    open(&mut server, URI, 5, VALID);
    for version in [4, 5] {
        assert!(change(&mut server, version, json!([{"text": "bad"}])).is_empty());
        assert!(open(&mut server, URI, version, "bad").is_empty());
    }
    assert_eq!(
        query(
            &mut server,
            "textDocument/hover",
            URI,
            0,
            VALID.rfind('n').unwrap() as u32
        )["result"]["contents"]["value"],
        "n: Int"
    );
    assert_eq!(
        change(&mut server, 6, json!([{"text": VALID}]))[0]["params"]["version"],
        6
    );
}

#[test]
fn full_document_sync_uses_last_change_and_ignores_incremental_edits() {
    let mut server = server();
    open(&mut server, URI, 1, VALID);
    let messages = change(&mut server, 2, json!([{"text": "bad"}, {"text": VALID}]));
    assert_eq!(messages[0]["params"]["diagnostics"], json!([]));
    let ranged = json!([{"range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}, "text": "bad"}]);
    assert!(change(&mut server, 3, ranged).is_empty());
    assert!(change(&mut server, 3, json!([])).is_empty());
    // Rejected changes must not consume the version number.
    assert_eq!(
        change(&mut server, 3, json!([{"text": VALID}]))[0]["params"]["version"],
        3
    );
}

#[test]
fn unopened_or_malformed_document_updates_are_ignored() {
    let mut server = server();
    assert!(change(&mut server, 1, json!([{"text": VALID}])).is_empty());
    for params in [
        json!({}),
        json!({"textDocument": {"uri": URI, "text": VALID}}),
        json!({"textDocument": {"uri": URI, "version": 1}}),
        json!({"textDocument": {"uri": URI, "version": "1", "text": VALID}}),
    ] {
        assert!(
            server
                .handle(&notification("textDocument/didOpen", params))
                .is_empty()
        );
    }
    assert_eq!(open(&mut server, URI, 1, VALID).len(), 1);
}

#[test]
fn close_clears_diagnostics_and_forgets_document_until_reopened() {
    let mut server = server();
    open(&mut server, URI, 10, VALID);
    let messages = server.handle(&notification(
        "textDocument/didClose",
        json!({"textDocument": {"uri": URI}}),
    ));
    assert_eq!(
        messages[0]["params"],
        json!({"uri": URI, "diagnostics": []})
    );
    assert_eq!(
        query(&mut server, "textDocument/hover", URI, 0, 0)["error"]["code"],
        -32602
    );
    assert!(change(&mut server, 11, json!([{"text": VALID}])).is_empty());
    assert_eq!(open(&mut server, URI, 1, VALID).len(), 1);
}

#[test]
fn multiple_open_documents_keep_analysis_and_definition_uris_separate() {
    let mut server = server();
    let other = "file:///other.wings";
    open(&mut server, URI, 1, VALID);
    let text = "process Q { state n: Bool = true; on Get -> Bool { n } }";
    open(&mut server, other, 1, text);
    assert_eq!(
        query(
            &mut server,
            "textDocument/hover",
            URI,
            0,
            VALID.rfind('n').unwrap() as u32
        )["result"]["contents"]["value"],
        "n: Int"
    );
    assert_eq!(
        query(
            &mut server,
            "textDocument/hover",
            other,
            0,
            text.rfind('n').unwrap() as u32
        )["result"]["contents"]["value"],
        "n: Bool"
    );
    assert_eq!(
        query(
            &mut server,
            "textDocument/definition",
            other,
            0,
            text.rfind('n').unwrap() as u32
        )["result"]["uri"],
        other
    );
}

#[test]
fn incomplete_document_keeps_later_handler_queries_available() {
    let mut server = server();
    let text = "process P { state n: Int = 0; on Bad -> Int { + } on Good -> Int { n } }";
    let messages = open(&mut server, URI, 1, text);
    assert!(
        !messages[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        query(
            &mut server,
            "textDocument/hover",
            URI,
            0,
            text.find("Good").unwrap() as u32
        )["result"]["contents"]["value"],
        "Good: () -> Int"
    );
}

#[test]
fn run_frames_multiple_messages_and_exits_cleanly_only_after_shutdown() {
    let (clean, responses) = exchange(&[
        request(json!(1), "initialize", json!({})),
        notification(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": URI, "version": 1, "text": VALID}}),
        ),
        request(json!(2), "shutdown", Value::Null),
        notification("exit", Value::Null),
        request(json!(3), "notReadAfterExit", Value::Null),
    ]);
    assert!(clean);
    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[1]["method"], "textDocument/publishDiagnostics");
    assert_eq!(
        responses[2],
        json!({"jsonrpc": "2.0", "id": 2, "result": null})
    );
    let (clean, responses) = exchange(&[notification("exit", Value::Null)]);
    assert!(!clean);
    assert!(responses.is_empty());
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
