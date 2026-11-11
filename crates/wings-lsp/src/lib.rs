mod transport;

use std::collections::HashMap;
use std::io::{self, BufRead, Write};

use serde_json::{Value, json};
use wings_sema::{Analysis, SymbolKind, analyze};
use wings_source::{Position, SourceFile, SourceId, Span};

struct Document {
    version: i64,
    source: SourceFile,
    analysis: Analysis,
}

#[derive(Default)]
pub struct Server {
    initialized: bool,
    shutdown: bool,
    documents: HashMap<String, Document>,
}

impl Server {
    pub fn handle(&mut self, message: &Value) -> Vec<Value> {
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || !message.is_object()
            || message
                .get("id")
                .is_some_and(|id| !id.is_null() && !id.is_string() && !id.is_number())
        {
            return vec![error(&Value::Null, -32600, "invalid JSON-RPC request")];
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Vec::new();
        };
        let id = message.get("id");
        let params = &message["params"];
        if method == "initialize" && !self.initialized && !self.shutdown {
            let Some(id) = id else {
                return Vec::new();
            };
            if let Some(encodings) = params
                .pointer("/capabilities/general/positionEncodings")
                .and_then(Value::as_array)
            {
                if !encodings.iter().any(|encoding| encoding == "utf-16") {
                    return vec![error(
                        id,
                        -32602,
                        "this prototype requires UTF-16 positions",
                    )];
                }
            }
            self.initialized = true;
            return vec![response(
                id,
                json!({
                    "capabilities": {
                        "positionEncoding": "utf-16",
                        "textDocumentSync": { "openClose": true, "change": 1 },
                        "hoverProvider": true,
                        "definitionProvider": true,
                        "completionProvider": { "resolveProvider": false }
                    },
                    "serverInfo": { "name": "wings-lsp", "version": env!("CARGO_PKG_VERSION") }
                }),
            )];
        }
        if !self.initialized {
            return id
                .map(|id| vec![error(id, -32002, "server not initialized")])
                .unwrap_or_default();
        }
        if method == "shutdown" && !self.shutdown {
            if let Some(id) = id {
                self.shutdown = true;
                return vec![response(id, Value::Null)];
            }
            return Vec::new();
        }
        if self.shutdown {
            return id
                .map(|id| vec![error(id, -32600, "server has shut down")])
                .unwrap_or_default();
        }
        match method {
            "textDocument/didOpen" | "textDocument/didChange" => self.update(method, params),
            "textDocument/didClose" => {
                if let Some(uri) = params.pointer("/textDocument/uri").and_then(Value::as_str) {
                    self.documents.remove(uri);
                    vec![
                        json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": { "uri": uri, "diagnostics": [] } }),
                    ]
                } else {
                    Vec::new()
                }
            }
            "initialized" | "$/cancelRequest" | "$/setTrace" => Vec::new(),
            "textDocument/hover" | "textDocument/definition" | "textDocument/completion" => {
                let Some(id) = id else {
                    return Vec::new();
                };
                let Some((uri, document, offset)) = self.query(params) else {
                    return vec![error(
                        id,
                        -32602,
                        "expected an open document and valid UTF-16 position",
                    )];
                };
                let result = match method {
                    "textDocument/hover" => document.analysis.symbol_at(offset).map_or(Value::Null, |symbol| json!({
                        "contents": { "kind": "plaintext", "value": format!("{}: {}", symbol.name, symbol.ty) }
                    })),
                    "textDocument/definition" => document.analysis.symbol_at(offset).map_or(Value::Null, |symbol| json!({
                        "uri": uri, "range": range(&document.source, symbol.span)
                    })),
                    _ => {
                        let mut labels = HashMap::new();
                        for keyword in ["process", "state", "on", "let", "true", "false"] {
                            labels.insert(keyword.to_owned(), json!({ "label": keyword, "kind": 14 }));
                        }
                        for ty in ["Int", "Bool", "Unit"] {
                            labels.insert(ty.to_owned(), json!({ "label": ty, "kind": 7 }));
                        }
                        for symbol in document.analysis.visible_symbols(offset) {
                            let kind = match symbol.kind { SymbolKind::Process => 7, SymbolKind::Handler => 3, _ => 6 };
                            labels.insert(symbol.name.clone(), json!({ "label": symbol.name, "kind": kind, "detail": symbol.ty.to_string() }));
                        }
                        let mut items: Vec<Value> = labels.into_values().collect();
                        items.sort_by(|left, right| left["label"].as_str().cmp(&right["label"].as_str()));
                        json!({ "isIncomplete": false, "items": items })
                    }
                };
                vec![response(id, result)]
            }
            _ => id
                .map(|id| vec![error(id, -32601, "method not found")])
                .unwrap_or_default(),
        }
    }

    fn update(&mut self, method: &str, params: &Value) -> Vec<Value> {
        let Some(uri) = params.pointer("/textDocument/uri").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(version) = params
            .pointer("/textDocument/version")
            .and_then(Value::as_i64)
        else {
            return Vec::new();
        };
        if self
            .documents
            .get(uri)
            .is_some_and(|document| document.version >= version)
        {
            return Vec::new();
        }
        let text = if method == "textDocument/didOpen" {
            params.pointer("/textDocument/text").and_then(Value::as_str)
        } else {
            if !self.documents.contains_key(uri) {
                return Vec::new();
            }
            let Some(changes) = params["contentChanges"].as_array() else {
                return Vec::new();
            };
            if changes.iter().any(|change| change.get("range").is_some()) {
                return Vec::new();
            }
            changes.last().and_then(|change| change["text"].as_str())
        };
        let Some(text) = text else {
            return Vec::new();
        };
        let source = SourceFile::new(SourceId(0), text);
        let analysis = analyze(text);
        let diagnostics: Vec<Value> = analysis
            .diagnostics
            .iter()
            .map(|diagnostic| {
                json!({
                    "range": range(&source, diagnostic.span), "severity": 1,
                    "code": diagnostic.code, "source": "wings", "message": diagnostic.message
                })
            })
            .collect();
        self.documents.insert(
            uri.to_owned(),
            Document {
                version,
                source,
                analysis,
            },
        );
        vec![
            json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": { "uri": uri, "version": version, "diagnostics": diagnostics } }),
        ]
    }

    fn query(&self, params: &Value) -> Option<(&str, &Document, usize)> {
        let uri = params.pointer("/textDocument/uri")?.as_str()?;
        let (uri, document) = self.documents.get_key_value(uri)?;
        let position = Position {
            line: u32::try_from(params.pointer("/position/line")?.as_u64()?).ok()?,
            character: u32::try_from(params.pointer("/position/character")?.as_u64()?).ok()?,
        };
        Some((uri, document, document.source.offset(position)?))
    }
}

fn range(source: &SourceFile, span: Span) -> Value {
    let start = source.position(span.start).unwrap_or_default();
    let end = source.position(span.end).unwrap_or(start);
    json!({ "start": { "line": start.line, "character": start.character }, "end": { "line": end.line, "character": end.character } })
}

fn response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub fn run(mut reader: impl BufRead, mut writer: impl Write) -> io::Result<bool> {
    let mut server = Server::default();
    while let Some(message) = transport::read_message(&mut reader)? {
        if message["method"] == "exit" {
            return Ok(server.shutdown);
        }
        for response in server.handle(&message) {
            transport::write_message(&mut writer, &response)?;
        }
    }
    Ok(false)
}
