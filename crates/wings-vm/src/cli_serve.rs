use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wings_abi::{MAX_RECORD_REPLY_NODES, RecordSchema, ValueType};
use wings_vm::{
    MessageValue, RequestOutcome, ResourceAccount, Resources, ScalarModule, SchedulerHandle,
    SchedulerPid,
};

use crate::{cli, cli_io};

const MAX_LINE_BYTES: usize = 64 << 10;
const MAX_HTTP_HEADERS_BYTES: usize = 8 << 10;
const MAX_HTTP_BODY_BYTES: usize = 64 << 10;
const MAX_RESPONSE_BYTES: usize = 128 << 10;
const MAX_CONCURRENT_CONNECTIONS: usize = 16;
const CONNECTION_THREAD_STACK: usize = 512 << 10;
const CONNECTION_QUOTA_BYTES: u64 = 2 << 20;
const SOCKET_IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcEnvelope {
    message: String,
    #[serde(default)]
    args: Vec<Value>,
}

#[derive(Serialize)]
struct ErrorResponse<'msg> {
    error: &'msg str,
}

pub fn serve_stdio(
    cli_account: &ResourceAccount,
    entry_module: &ScalarModule,
    entry_pid: SchedulerPid,
    handle: &SchedulerHandle,
    max_steps: u64,
) -> Result<(), String> {
    let adapter = cli::Adapter::new(cli_account)?;
    let mut stdio = cli_io::Stdio::new(cli_account)?;
    let (reader, writer) = stdio.streams();
    let (mut line, _line_charge) = reserve_vec(cli_account, MAX_LINE_BYTES)?;
    let (mut out, _out_charge) = reserve_vec(cli_account, MAX_RESPONSE_BYTES)?;
    while read_bounded_line(reader, &mut line, MAX_LINE_BYTES)? {
        let trimmed = trim_ascii(&line);
        if trimmed.is_empty() {
            continue;
        }
        let rpc: RpcEnvelope = serde_json::from_slice(trimmed).map_err(|e| e.to_string())?;
        out.clear();
        execute_rpc(
            cli_account,
            &adapter,
            entry_module,
            entry_pid,
            handle,
            max_steps,
            &rpc.message,
            &rpc.args,
            &mut out,
        )?;
        writer.write_all(&out).map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn serve_tcp(
    addr: SocketAddr,
    app_tenant: &ResourceAccount,
    entry_module: &ScalarModule,
    entry_pid: SchedulerPid,
    handle: &SchedulerHandle,
    max_steps: u64,
) -> Result<(), String> {
    let listener =
        TcpListener::bind(addr).map_err(|e| format!("failed to bind TCP {addr}: {e}"))?;
    let local_addr = listener.local_addr().map_err(|e| e.to_string())?;
    announce_listening("tcp", local_addr)?;
    let active = Arc::new(AtomicUsize::new(0));
    for incoming in listener.incoming() {
        let mut stream = match incoming {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CONCURRENT_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            let _ = stream.write_all(b"{\"error\":\"server_busy\"}\n");
            continue;
        }
        let conn_account = match app_tenant.process_accounted(Resources {
            bytes: CONNECTION_QUOTA_BYTES,
            ..Resources::default()
        }) {
            Ok(acc) => acc,
            Err(_) => {
                active.fetch_sub(1, Ordering::AcqRel);
                let _ = stream.write_all(b"{\"error\":\"resource_exhausted\"}\n");
                continue;
            }
        };
        let thread_charge = match conn_account.reserve(Resources {
            bytes: (CONNECTION_THREAD_STACK + 65536) as u64,
            ..Resources::default()
        }) {
            Ok(charge) => charge,
            Err(_) => {
                active.fetch_sub(1, Ordering::AcqRel);
                let _ = stream.write_all(b"{\"error\":\"resource_exhausted\"}\n");
                continue;
            }
        };
        let module = entry_module.clone();
        let handle = handle.clone();
        let active_clone = active.clone();
        let spawned = thread::Builder::new()
            .name("wings-tcp-conn".into())
            .stack_size(CONNECTION_THREAD_STACK)
            .spawn(move || {
                let _guard = ConnectionGuard(active_clone);
                let _thread_charge = thread_charge;
                let _ = handle_tcp_connection(
                    stream,
                    &conn_account,
                    &module,
                    entry_pid,
                    &handle,
                    max_steps,
                );
            });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::AcqRel);
        }
    }
    Ok(())
}

pub fn serve_http(
    addr: SocketAddr,
    app_tenant: &ResourceAccount,
    entry_module: &ScalarModule,
    entry_pid: SchedulerPid,
    handle: &SchedulerHandle,
    max_steps: u64,
) -> Result<(), String> {
    let listener =
        TcpListener::bind(addr).map_err(|e| format!("failed to bind HTTP {addr}: {e}"))?;
    let local_addr = listener.local_addr().map_err(|e| e.to_string())?;
    announce_listening("http", local_addr)?;
    let active = Arc::new(AtomicUsize::new(0));
    for incoming in listener.incoming() {
        let mut stream = match incoming {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        if active.fetch_add(1, Ordering::AcqRel) >= MAX_CONCURRENT_CONNECTIONS {
            active.fetch_sub(1, Ordering::AcqRel);
            let _ = write_http_response(
                &mut stream,
                503,
                "Service Unavailable",
                b"{\"error\":\"server_busy\"}\n",
                true,
            );
            continue;
        }
        let conn_account = match app_tenant.process_accounted(Resources {
            bytes: CONNECTION_QUOTA_BYTES,
            ..Resources::default()
        }) {
            Ok(acc) => acc,
            Err(_) => {
                active.fetch_sub(1, Ordering::AcqRel);
                let _ = write_http_response(
                    &mut stream,
                    503,
                    "Service Unavailable",
                    b"{\"error\":\"resource_exhausted\"}\n",
                    true,
                );
                continue;
            }
        };
        let thread_charge = match conn_account.reserve(Resources {
            bytes: (CONNECTION_THREAD_STACK + 65536) as u64,
            ..Resources::default()
        }) {
            Ok(charge) => charge,
            Err(_) => {
                active.fetch_sub(1, Ordering::AcqRel);
                let _ = write_http_response(
                    &mut stream,
                    503,
                    "Service Unavailable",
                    b"{\"error\":\"resource_exhausted\"}\n",
                    true,
                );
                continue;
            }
        };
        let module = entry_module.clone();
        let handle = handle.clone();
        let active_clone = active.clone();
        let spawned = thread::Builder::new()
            .name("wings-http-conn".into())
            .stack_size(CONNECTION_THREAD_STACK)
            .spawn(move || {
                let _guard = ConnectionGuard(active_clone);
                let _thread_charge = thread_charge;
                let _ = handle_http_connection(
                    stream,
                    &conn_account,
                    &module,
                    entry_pid,
                    &handle,
                    max_steps,
                );
            });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::AcqRel);
        }
    }
    Ok(())
}

struct ConnectionGuard(Arc<AtomicUsize>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn announce_listening(scheme: &str, addr: SocketAddr) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{{\"listening\":\"{scheme}://{addr}\"}}")
        .and_then(|_| stdout.flush())
        .map_err(|e| e.to_string())
}

fn configure_stream(stream: &TcpStream) -> Result<(), String> {
    let _ = stream.set_nodelay(true);
    stream
        .set_read_timeout(Some(SOCKET_IO_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(SOCKET_IO_TIMEOUT))
        .map_err(|e| e.to_string())
}

fn handle_tcp_connection(
    stream: TcpStream,
    account: &ResourceAccount,
    entry_module: &ScalarModule,
    entry_pid: SchedulerPid,
    handle: &SchedulerHandle,
    max_steps: u64,
) -> Result<(), String> {
    configure_stream(&stream)?;
    let adapter = cli::Adapter::new(account)?;
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    let _reader_charge = account.reserve(Resources {
        bytes: 8192,
        ..Resources::default()
    })?;
    let mut reader = BufReader::with_capacity(8192, stream);
    let (mut line, _line_charge) = reserve_vec(account, MAX_LINE_BYTES)?;
    let (mut out, _out_charge) = reserve_vec(account, MAX_RESPONSE_BYTES)?;
    while read_bounded_line(&mut reader, &mut line, MAX_LINE_BYTES)? {
        let trimmed = trim_ascii(&line);
        if trimmed.is_empty() {
            continue;
        }
        out.clear();
        let rpc_res: Result<RpcEnvelope, String> =
            serde_json::from_slice(trimmed).map_err(|e| e.to_string());
        match rpc_res.and_then(|rpc| {
            execute_rpc(
                account,
                &adapter,
                entry_module,
                entry_pid,
                handle,
                max_steps,
                &rpc.message,
                &rpc.args,
                &mut out,
            )
        }) {
            Ok(()) => {
                writer.write_all(&out).map_err(|e| e.to_string())?;
                writer.flush().map_err(|e| e.to_string())?;
            }
            Err(err) => {
                out.clear();
                serde_json::to_writer(&mut out, &ErrorResponse { error: &err })
                    .map_err(|e| e.to_string())?;
                out.push(b'\n');
                writer.write_all(&out).map_err(|e| e.to_string())?;
                writer.flush().map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

fn handle_http_connection(
    stream: TcpStream,
    account: &ResourceAccount,
    entry_module: &ScalarModule,
    entry_pid: SchedulerPid,
    handle: &SchedulerHandle,
    max_steps: u64,
) -> Result<(), String> {
    configure_stream(&stream)?;
    let adapter = cli::Adapter::new(account)?;
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    let _reader_charge = account.reserve(Resources {
        bytes: 8192,
        ..Resources::default()
    })?;
    let mut reader = BufReader::with_capacity(8192, stream);
    let (mut headers_buf, _hdr_charge) = reserve_vec(account, MAX_HTTP_HEADERS_BYTES)?;
    let (mut body_buf, _body_charge) = reserve_vec(account, MAX_HTTP_BODY_BYTES)?;
    let (mut out, _out_charge) = reserve_vec(account, MAX_RESPONSE_BYTES)?;
    loop {
        headers_buf.clear();
        match read_http_headers(&mut reader, &mut headers_buf) {
            Ok(false) => break,
            Ok(true) => {}
            Err(err) => {
                out.clear();
                let _ = serde_json::to_writer(&mut out, &ErrorResponse { error: &err });
                out.push(b'\n');
                let _ = write_http_response(&mut writer, 400, "Bad Request", &out, true);
                break;
            }
        }
        let header_str = match std::str::from_utf8(&headers_buf) {
            Ok(s) => s,
            Err(_) => {
                let _ = write_http_response(
                    &mut writer,
                    400,
                    "Bad Request",
                    b"{\"error\":\"invalid UTF-8 HTTP headers\"}\n",
                    true,
                );
                break;
            }
        };
        let mut lines = header_str.split("\r\n");
        let request_line = lines.next().unwrap_or("");
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let raw_target = parts.next().unwrap_or("");
        let version = parts.next().unwrap_or("");
        if method.is_empty() || raw_target.is_empty() || !version.starts_with("HTTP/1.") {
            let _ = write_http_response(
                &mut writer,
                400,
                "Bad Request",
                b"{\"error\":\"malformed HTTP request line\"}\n",
                true,
            );
            break;
        }
        let mut content_length: usize = 0;
        let mut close_conn = version == "HTTP/1.0";
        let mut bad_header = None;
        for line in lines {
            if line.is_empty() {
                break;
            }
            let Some((name, val)) = line.split_once(':') else {
                bad_header = Some("malformed HTTP header");
                break;
            };
            let val = val.trim();
            if name.eq_ignore_ascii_case("content-length") {
                match val.parse::<usize>() {
                    Ok(len) if len <= MAX_HTTP_BODY_BYTES => content_length = len,
                    _ => {
                        bad_header = Some("invalid or oversized Content-Length");
                        break;
                    }
                }
            } else if name.eq_ignore_ascii_case("connection") {
                if val.eq_ignore_ascii_case("close") {
                    close_conn = true;
                } else if val.eq_ignore_ascii_case("keep-alive") {
                    close_conn = false;
                }
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                bad_header = Some("chunked Transfer-Encoding is not supported; use Content-Length");
                break;
            }
        }
        if let Some(msg) = bad_header {
            out.clear();
            let _ = serde_json::to_writer(&mut out, &ErrorResponse { error: msg });
            out.push(b'\n');
            let _ = write_http_response(&mut writer, 400, "Bad Request", &out, true);
            break;
        }
        body_buf.clear();
        body_buf.resize(content_length, 0);
        if content_length > 0 && reader.read_exact(&mut body_buf).is_err() {
            break;
        }
        let path = raw_target.split('?').next().unwrap_or(raw_target);
        if method == "GET" && path == "/healthz" {
            write_http_response(&mut writer, 200, "OK", b"{\"ok\":true}\n", close_conn)?;
            if close_conn {
                break;
            }
            continue;
        }
        if method != "POST" {
            write_http_response(
                &mut writer,
                404,
                "Not Found",
                b"{\"error\":\"not_found\"}\n",
                close_conn,
            )?;
            if close_conn {
                break;
            }
            continue;
        }
        let parsed_rpc: Result<(String, Vec<Value>), String> = if path == "/" {
            serde_json::from_slice::<RpcEnvelope>(&body_buf)
                .map(|rpc| (rpc.message, rpc.args))
                .map_err(|e| e.to_string())
        } else {
            let handler_name = path.trim_start_matches('/');
            if handler_name.is_empty() || handler_name.contains('/') {
                Err("not_found".into())
            } else {
                let trimmed = trim_ascii(&body_buf);
                if trimmed.is_empty() {
                    Ok((handler_name.to_string(), Vec::new()))
                } else if trimmed.first() == Some(&b'[') {
                    serde_json::from_slice::<Vec<Value>>(trimmed)
                        .map(|args| (handler_name.to_string(), args))
                        .map_err(|e| e.to_string())
                } else {
                    serde_json::from_slice::<Value>(trimmed)
                        .map_err(|e| e.to_string())
                        .and_then(|val| match val {
                            Value::Object(mut map)
                                if map.len() == 1
                                    && map.get("args").is_some_and(Value::is_array) =>
                            {
                                let arr = match map.remove("args") {
                                    Some(Value::Array(items)) => items,
                                    _ => unreachable!(),
                                };
                                Ok((handler_name.to_string(), arr))
                            }
                            Value::Object(map) if map.is_empty() => {
                                Ok((handler_name.to_string(), Vec::new()))
                            }
                            Value::Object(map) => {
                                Ok((handler_name.to_string(), vec![Value::Object(map)]))
                            }
                            _ => Err("expected JSON array or object body".into()),
                        })
                }
            }
        };
        out.clear();
        match parsed_rpc {
            Err(err) if err == "not_found" => {
                write_http_response(
                    &mut writer,
                    404,
                    "Not Found",
                    b"{\"error\":\"not_found\"}\n",
                    close_conn,
                )?;
            }
            Err(err) => {
                let _ = serde_json::to_writer(&mut out, &ErrorResponse { error: &err });
                out.push(b'\n');
                write_http_response(&mut writer, 400, "Bad Request", &out, close_conn)?;
            }
            Ok((msg_name, args)) => {
                match execute_rpc(
                    account,
                    &adapter,
                    entry_module,
                    entry_pid,
                    handle,
                    max_steps,
                    &msg_name,
                    &args,
                    &mut out,
                ) {
                    Ok(()) => {
                        write_http_response(&mut writer, 200, "OK", &out, close_conn)?;
                    }
                    Err(err) => {
                        let (code, status_text) = if err.starts_with("unknown handler ") {
                            (404, "Not Found")
                        } else if err == "request timed out" {
                            (504, "Gateway Timeout")
                        } else if err == "managed actor handler failed"
                            || err == "request cancelled"
                        {
                            (500, "Internal Server Error")
                        } else {
                            (400, "Bad Request")
                        };
                        out.clear();
                        let _ = serde_json::to_writer(&mut out, &ErrorResponse { error: &err });
                        out.push(b'\n');
                        write_http_response(&mut writer, code, status_text, &out, close_conn)?;
                    }
                }
            }
        }
        if close_conn {
            break;
        }
    }
    Ok(())
}

fn write_http_response(
    writer: &mut impl Write,
    status: u16,
    reason: &str,
    body: &[u8],
    close: bool,
) -> Result<(), String> {
    let conn = if close { "close" } else { "keep-alive" };
    write!(
        writer,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {conn}\r\n\r\n",
        body.len()
    )
    .and_then(|_| writer.write_all(body))
    .and_then(|_| writer.flush())
    .map_err(|e| e.to_string())
}

fn read_http_headers(reader: &mut impl BufRead, out: &mut Vec<u8>) -> Result<bool, String> {
    loop {
        let available = reader.fill_buf().map_err(|e| e.to_string())?;
        if available.is_empty() {
            return if out.is_empty() {
                Ok(false)
            } else {
                Err("unexpected EOF in HTTP headers".into())
            };
        }
        let prev_tail = out.len().saturating_sub(3);
        let max_take = MAX_HTTP_HEADERS_BYTES
            .checked_sub(out.len())
            .ok_or("HTTP headers exceed 8 KiB")?;
        if max_take == 0 {
            return Err("HTTP headers exceed 8 KiB".into());
        }
        let scan_len = available.len().min(max_take);
        out.extend_from_slice(&available[..scan_len]);
        if let Some(rel) = out[prev_tail..]
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
        {
            let header_end = prev_tail + rel + 4;
            let consumed_from_available = header_end - (out.len() - scan_len);
            out.truncate(header_end);
            reader.consume(consumed_from_available);
            return Ok(true);
        }
        reader.consume(scan_len);
        if out.len() == MAX_HTTP_HEADERS_BYTES {
            return Err("HTTP headers exceed 8 KiB".into());
        }
    }
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
    limit: usize,
) -> Result<bool, String> {
    line.clear();
    loop {
        let buffer = reader.fill_buf().map_err(|e| e.to_string())?;
        if buffer.is_empty() {
            return Ok(!line.is_empty());
        }
        let length = buffer
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buffer.len(), |idx| idx + 1);
        if length > limit - line.len() {
            return Err("input line exceeds 64 KiB".into());
        }
        let finished = buffer[length - 1] == b'\n';
        line.extend_from_slice(&buffer[..length]);
        reader.consume(length);
        if finished {
            return Ok(true);
        }
    }
}

fn reserve_vec(
    account: &ResourceAccount,
    capacity: usize,
) -> Result<(Vec<u8>, wings_vm::Reservation), String> {
    let charge = account.reserve(Resources {
        bytes: capacity as u64,
        ..Resources::default()
    })?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(capacity).map_err(|e| e.to_string())?;
    Ok((buf, charge))
}

fn trim_ascii(mut slice: &[u8]) -> &[u8] {
    while let Some((first, rest)) = slice.split_first() {
        if first.is_ascii_whitespace() {
            slice = rest;
        } else {
            break;
        }
    }
    while let Some((last, rest)) = slice.split_last() {
        if last.is_ascii_whitespace() {
            slice = rest;
        } else {
            break;
        }
    }
    slice
}

#[allow(clippy::too_many_arguments)]
fn execute_rpc(
    account: &ResourceAccount,
    adapter: &cli::Adapter,
    entry_module: &ScalarModule,
    entry_pid: SchedulerPid,
    handle: &SchedulerHandle,
    max_steps: u64,
    message_name: &str,
    raw_args: &[Value],
    out: &mut Vec<u8>,
) -> Result<(), String> {
    let handler = entry_module.handler(message_name)?;
    let schema = handler.schema();
    let reply_type = schema.reply;
    let records = &entry_module.manifest().records;
    let mut leaves: [(ValueType, &Value); 64] = [(ValueType::Unit, &Value::Null); 64];
    let mut leaf_count = 0usize;
    if raw_args.len() == schema.parameters.len()
        && schema
            .parameters
            .iter()
            .zip(raw_args)
            .all(|((_, ty), val)| !is_struct_object_for_flat_leaf(*ty, val))
    {
        let mut node_budget = MAX_RECORD_REPLY_NODES;
        for ((_, ty), val) in schema.parameters.iter().zip(raw_args) {
            expand_json_param(
                records,
                *ty,
                val,
                &mut leaves,
                &mut leaf_count,
                &mut node_budget,
            )?;
        }
    } else {
        match_flattened_args(
            records,
            &schema.parameters,
            raw_args,
            &mut leaves,
            &mut leaf_count,
        )?;
    }

    let mut list_storage = [0i64; 64];
    let mut list_used = 0usize;
    let mut byte_count = 0usize;
    for (ty, val) in &leaves[..leaf_count] {
        match ty {
            ValueType::StringRef => {
                let s = val.as_str().ok_or("expected a String argument")?;
                if s.len() > 65536 - byte_count {
                    return Err("message strings and bytes exceed 65536 total bytes".into());
                }
                byte_count += s.len();
            }
            ValueType::BytesRef => {
                let arr = extract_bytes_array(val)?;
                if arr.len() > 65536 - byte_count {
                    return Err("message strings and bytes exceed 65536 total bytes".into());
                }
                byte_count += arr.len();
            }
            _ => {}
        }
    }
    let _byte_charge = if byte_count > 0 {
        Some(account.reserve(Resources {
            bytes: byte_count as u64,
            ..Resources::default()
        })?)
    } else {
        None
    };
    let mut byte_storage = Vec::new();
    if byte_count > 0 {
        byte_storage
            .try_reserve_exact(byte_count)
            .map_err(|e| e.to_string())?;
    }
    let mut segments = [(0usize, 0usize); 64];
    for (idx, (ty, val)) in leaves[..leaf_count].iter().enumerate() {
        match ty {
            ValueType::ListRef => {
                let arr = val.as_array().ok_or("expected an IntList array argument")?;
                if arr.len() > 64 - list_used {
                    return Err("message lists exceed 64 total integers".into());
                }
                let start = list_used;
                for item in arr {
                    let n = item.as_i64().ok_or("expected i64 elements in IntList")?;
                    list_storage[list_used] = n;
                    list_used += 1;
                }
                segments[idx] = (start, arr.len());
            }
            ValueType::BytesRef => {
                let arr = extract_bytes_array(val)?;
                let start = byte_storage.len();
                for item in arr {
                    let b = item
                        .as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or("expected 0..=255 byte elements")?;
                    byte_storage.push(b);
                }
                segments[idx] = (start, arr.len());
            }
            _ => {}
        }
    }

    let mut message_values = [MessageValue::Unit; 64];
    for (idx, (ty, val)) in leaves[..leaf_count].iter().enumerate() {
        message_values[idx] = match ty {
            ValueType::Int => MessageValue::Int(val.as_i64().ok_or("expected an i64 argument")?),
            ValueType::Bool => MessageValue::Bool(val.as_bool().ok_or("expected a Bool argument")?),
            ValueType::Unit => {
                if !val.is_null() {
                    return Err("expected null for Unit".into());
                }
                MessageValue::Unit
            }
            ValueType::StringRef => {
                MessageValue::String(val.as_str().ok_or("expected a String argument")?)
            }
            ValueType::ListRef => {
                let (start, len) = segments[idx];
                MessageValue::IntList(&list_storage[start..start + len])
            }
            ValueType::BytesRef => {
                let (start, len) = segments[idx];
                MessageValue::Bytes(&byte_storage[start..start + len])
            }
            ValueType::Ref => {
                let n = val
                    .as_object()
                    .filter(|obj| obj.len() == 1)
                    .and_then(|obj| obj.get("record_int"))
                    .and_then(Value::as_i64)
                    .ok_or("expected {\"record_int\": <i64>} argument")?;
                MessageValue::RecordInt(n)
            }
            ValueType::RecordRef(_) => unreachable!("records expanded to leaves"),
            ValueType::Pid | ValueType::RequestId => {
                return Err("process capabilities are not JSON arguments".into());
            }
        };
    }

    let values = &message_values[..leaf_count];
    if reply_type.is_none() {
        handle.send_values(entry_pid, &handler, values, max_steps)?;
        adapter.write_reply(out, None, None)?;
        return Ok(());
    }
    let mut pending = handle.request_values(
        entry_pid,
        &handler,
        values,
        max_steps,
        Duration::from_secs(30),
    )?;
    match pending.wait(Duration::from_secs(30))? {
        Some(RequestOutcome::Reply(val)) => adapter.write_reply(out, reply_type, Some(val)),
        Some(RequestOutcome::ReplyString(val)) => adapter.write_string_reply(out, &val),
        Some(RequestOutcome::ReplyBytes(val)) => adapter.write_bytes_reply(out, &val),
        Some(RequestOutcome::ReplyList(val)) => adapter.write_list_reply(out, &val),
        Some(RequestOutcome::ReplyRecord(val)) => adapter.write_record_reply(out, &val),
        Some(RequestOutcome::TimedOut) | None => Err("request timed out".into()),
        Some(RequestOutcome::Cancelled) => Err("request cancelled".into()),
        Some(RequestOutcome::Failed) => Err("managed actor handler failed".into()),
    }
}

fn extract_bytes_array(val: &Value) -> Result<&[Value], String> {
    if let Some(arr) = val.as_array() {
        return Ok(arr);
    }
    if let Some(obj) = val.as_object()
        && obj.len() == 1
        && let Some(arr) = obj.get("bytes").and_then(Value::as_array)
    {
        return Ok(arr);
    }
    Err("expected byte array or {\"bytes\": [...]} argument".into())
}

fn is_struct_object_for_flat_leaf(expected: ValueType, val: &Value) -> bool {
    let Some(obj) = val.as_object() else {
        return false;
    };
    match expected {
        ValueType::RecordRef(_) => false,
        ValueType::Ref => !(obj.len() == 1 && obj.contains_key("record_int")),
        ValueType::BytesRef => !(obj.len() == 1 && obj.contains_key("bytes")),
        _ => true,
    }
}

fn match_flattened_args<'val>(
    records: &[RecordSchema],
    params: &[(String, ValueType)],
    raw_args: &'val [Value],
    leaves: &mut [(ValueType, &'val Value); 64],
    leaf_count: &mut usize,
) -> Result<(), String> {
    let mut param_idx = 0usize;
    for val in raw_args {
        if param_idx >= params.len() {
            return Err("handler argument count mismatch".into());
        }
        let expected_ty = params[param_idx].1;
        if is_struct_object_for_flat_leaf(expected_ty, val) {
            let mut matched = false;
            for schema_idx in (0..records.len()).rev() {
                let mut trial_leaves = *leaves;
                let mut trial_count = *leaf_count;
                let mut budget = MAX_RECORD_REPLY_NODES;
                if expand_json_param(
                    records,
                    ValueType::RecordRef(schema_idx as u16),
                    val,
                    &mut trial_leaves,
                    &mut trial_count,
                    &mut budget,
                )
                .is_ok()
                {
                    let added = trial_count - *leaf_count;
                    if param_idx + added <= params.len()
                        && trial_leaves[*leaf_count..trial_count]
                            .iter()
                            .zip(&params[param_idx..param_idx + added])
                            .all(|((lty, _), (_, pty))| lty == pty)
                    {
                        *leaves = trial_leaves;
                        *leaf_count = trial_count;
                        param_idx += added;
                        matched = true;
                        break;
                    }
                }
            }
            if !matched {
                return Err(
                    "JSON struct argument does not match any manifest struct schema".into(),
                );
            }
        } else {
            let mut budget = MAX_RECORD_REPLY_NODES;
            expand_json_param(records, expected_ty, val, leaves, leaf_count, &mut budget)?;
            param_idx += 1;
        }
    }
    if param_idx != params.len() {
        return Err("handler argument count mismatch".into());
    }
    Ok(())
}

fn expand_json_param<'val>(
    records: &[RecordSchema],
    ty: ValueType,
    val: &'val Value,
    leaves: &mut [(ValueType, &'val Value); 64],
    leaf_count: &mut usize,
    node_budget: &mut usize,
) -> Result<(), String> {
    match ty {
        ValueType::RecordRef(index) => {
            if *node_budget == 0 {
                return Err("struct argument expansion exceeds 64 total nodes".into());
            }
            *node_budget -= 1;
            let schema = records
                .get(usize::from(index))
                .ok_or("unknown struct schema in manifest")?;
            let obj = val
                .as_object()
                .ok_or_else(|| format!("expected JSON object for struct '{}'", schema.name))?;
            if obj.len() != schema.fields.len() {
                return Err(format!(
                    "struct '{}' field count mismatch (expected {})",
                    schema.name,
                    schema.fields.len()
                ));
            }
            for (field_name, field_ty) in &schema.fields {
                let field_val = obj.get(field_name).ok_or_else(|| {
                    format!("missing field '{field_name}' in struct '{}'", schema.name)
                })?;
                expand_json_param(
                    records,
                    *field_ty,
                    field_val,
                    leaves,
                    leaf_count,
                    node_budget,
                )?;
            }
            Ok(())
        }
        other => {
            if *leaf_count >= 64 {
                return Err("expanded message parameters exceed 64 values".into());
            }
            leaves[*leaf_count] = (other, val);
            *leaf_count += 1;
            Ok(())
        }
    }
}
