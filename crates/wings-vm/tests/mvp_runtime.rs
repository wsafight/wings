#![cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
))]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use wings_compile::build_app;
use wings_vm::{
    MessageValue, RequestOutcome, ResourceAccount, Resources, ScalarModule, ScalarWorker,
    Scheduler, SchedulerConfig, WorkerEvent,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "wings-mvp-{prefix}-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn scalar_worker_supports_nullable_pid_state_send_after_and_monitor_restart() {
    let dir = TempDir::new("worker");
    let bundle_dir = dir.0.join("bundle");
    let source = include_str!("../../../examples/session-service.wings");
    build_app(source, &bundle_dir).unwrap();

    let limits = Resources {
        bytes: 64 << 20,
        processes: 32,
        messages: 128,
        requests: 64,
    };
    let vm = ResourceAccount::vm_accounted(limits).unwrap();
    let bootstrap = vm.used();
    {
        let tenant = vm.tenant_accounted(limits).unwrap();
        let (modules, entry_idx) =
            unsafe { ScalarModule::load_bundle_accounted(&bundle_dir, &vm) }.unwrap();
        let mut worker = ScalarWorker::new(&tenant, 16).unwrap();
        worker.enable_wait().unwrap();
        worker.register_modules(&modules).unwrap();
        let entry_mod = &modules[entry_idx];
        let manager = worker.spawn_with_heap(entry_mod, limits, 32, 64).unwrap();

        let req = worker
            .request(
                manager,
                &entry_mod.handler("boot").unwrap(),
                &[],
                100_000,
                Duration::from_secs(2),
            )
            .unwrap();
        drain_worker(&mut worker);
        assert_eq!(
            worker.take_reply(req).unwrap(),
            Some(RequestOutcome::Reply(1))
        );

        let wait_until = Instant::now() + Duration::from_millis(25);
        while Instant::now() < wait_until {
            let _ = worker
                .wait(wait_until.saturating_duration_since(Instant::now()))
                .unwrap();
            drain_worker(&mut worker);
        }

        // Dispatch normal job (SessionCommand { session_id: 7, prompt: "hello" })
        let req = worker
            .request_values(
                manager,
                &entry_mod.handler("dispatch").unwrap(),
                &[MessageValue::Int(7), MessageValue::String("hello")],
                100_000,
                Duration::from_secs(2),
            )
            .unwrap();
        drain_worker(&mut worker);
        assert_eq!(
            worker.take_reply(req).unwrap(),
            Some(RequestOutcome::Reply(1))
        );

        // Crash child with "boom" -> monitor triggers on_worker_down and auto-restarts!
        let req = worker
            .request_values(
                manager,
                &entry_mod.handler("dispatch").unwrap(),
                &[MessageValue::Int(7), MessageValue::String("boom")],
                100_000,
                Duration::from_secs(2),
            )
            .unwrap();
        drain_worker(&mut worker);
        assert_eq!(
            worker.take_reply(req).unwrap(),
            Some(RequestOutcome::Reply(1))
        );

        // Dispatch after restart -> new SessionWorker starts fresh at turn #1!
        let req = worker
            .request_values(
                manager,
                &entry_mod.handler("dispatch").unwrap(),
                &[MessageValue::Int(9), MessageValue::String("recovered")],
                100_000,
                Duration::from_secs(2),
            )
            .unwrap();
        drain_worker(&mut worker);
        assert_eq!(
            worker.take_reply(req).unwrap(),
            Some(RequestOutcome::Reply(1))
        );

        let status_req = worker
            .request(
                manager,
                &entry_mod.handler("status").unwrap(),
                &[],
                100_000,
                Duration::from_secs(2),
            )
            .unwrap();
        drain_worker(&mut worker);
        let Some(RequestOutcome::ReplyRecord(record)) = worker.take_reply(status_req).unwrap()
        else {
            panic!("expected SessionStatus record reply");
        };
        let vals: Vec<_> = record.values().collect();
        assert_eq!(
            vals,
            vec![
                MessageValue::Bool(true),
                MessageValue::Int(2),
                MessageValue::Int(1),
                MessageValue::Int(1),
                MessageValue::String("s9#1:recovered"),
            ]
        );
    }
    assert_eq!(vm.used(), bootstrap);
}

fn drain_worker(worker: &mut ScalarWorker) {
    for _ in 0..256 {
        if worker.tick(10_000).unwrap() == WorkerEvent::Idle {
            break;
        }
    }
}

#[test]
fn scheduler_and_serve_tcp_http_handle_pid_timers_monitors_and_rich_json_rpc() {
    let dir = TempDir::new("serve");
    let bundle_dir = dir.0.join("bundle");
    let source = include_str!("../../../examples/session-service.wings");
    build_app(source, &bundle_dir).unwrap();

    // 1. Direct multi-worker Scheduler check + exact zero-leak accounting on drop
    let limits = Resources {
        bytes: 128 << 20,
        processes: 64,
        messages: 256,
        requests: 64,
    };
    let vm = ResourceAccount::vm_accounted(limits).unwrap();
    let bootstrap = vm.used();
    {
        let tenant = vm.tenant_accounted(limits).unwrap();
        let (modules, entry_idx) =
            unsafe { ScalarModule::load_bundle_accounted(&bundle_dir, &vm) }.unwrap();
        let scheduler = Scheduler::new(
            &tenant,
            SchedulerConfig {
                workers: 2,
                capacity: 32,
                requests: 32,
                ..SchedulerConfig::default()
            },
        )
        .unwrap();
        let handle = scheduler.handle();
        handle.register_modules(&modules).unwrap();
        let entry_mod = &modules[entry_idx];
        let manager = handle.spawn_with_heap(entry_mod, limits, 32, 64).unwrap();
        let mut boot = handle
            .request(
                manager,
                &entry_mod.handler("boot").unwrap(),
                &[],
                100_000,
                Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(
            boot.wait(Duration::from_secs(2)).unwrap(),
            Some(RequestOutcome::Reply(1))
        );
        // Schedule a pending send_after and active monitor, then drop Scheduler cleanly to verify zero resource leaks
    }
    assert_eq!(vm.used(), bootstrap);

    // 2. Test `wings-vm serve --http 127.0.0.1:0` end-to-end
    let mut http_child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_wings-vm"))
            .args([
                "serve",
                "--trusted",
                bundle_dir.to_str().unwrap(),
                "--workers",
                "2",
                "--http",
                "127.0.0.1:0",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let http_addr = read_listening_addr(&mut http_child.0, "http://");
    let (code, health) = http_request(&http_addr, "GET", "/healthz", "");
    assert_eq!(code, 200);
    assert_eq!(health["ok"], Value::Bool(true));

    let (code, boot_resp) = http_request(&http_addr, "POST", "/boot", "");
    assert_eq!(code, 200);
    assert_eq!(boot_resp["reply"], Value::Bool(true));

    // Poll until bound_ok & heartbeat tick == 1
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let (code, st) = http_request(&http_addr, "POST", "/status", "[]");
        assert_eq!(code, 200);
        if st["reply"]["alive"] == true && st["reply"]["ticks"] == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for boot/tick: {st}"
        );
        thread::sleep(Duration::from_millis(5));
    }

    // Send struct argument over HTTP POST /dispatch
    let (code, disp1) = http_request(
        &http_addr,
        "POST",
        "/dispatch",
        r#"[{"session_id":42,"prompt":"first"}]"#,
    );
    assert_eq!(code, 200);
    assert_eq!(disp1["reply"], true);

    wait_for_http_status(&http_addr, |reply| {
        reply["turns"] == 1 && reply["last_reply"]["string"] == "s42#1:first"
    });

    // Trigger child crash ("boom") and verify monitor restarts child automatically!
    let (code, _) = http_request(
        &http_addr,
        "POST",
        "/",
        r#"{"message":"dispatch","args":[{"session_id":42,"prompt":"boom"}]}"#,
    );
    assert_eq!(code, 200);

    wait_for_http_status(&http_addr, |reply| {
        reply["restarts"] == 1 && reply["alive"] == true
    });

    // Send another step to the restarted worker -> turn resets to #1 on new child while manager turns == 2!
    let (code, _) = http_request(
        &http_addr,
        "POST",
        "/dispatch",
        r#"{"args":[{"session_id":42,"prompt":"after_restart"}]}"#,
    );
    assert_eq!(code, 200);

    wait_for_http_status(&http_addr, |reply| {
        reply["turns"] == 2
            && reply["restarts"] == 1
            && reply["last_reply"]["string"] == "s42#1:after_restart"
    });

    drop(http_child);

    // 3. Test `wings-vm serve --tcp 127.0.0.1:0` end-to-end
    let mut tcp_child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_wings-vm"))
            .args([
                "serve",
                "--trusted",
                bundle_dir.to_str().unwrap(),
                "--tcp",
                "127.0.0.1:0",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let tcp_addr = read_listening_addr(&mut tcp_child.0, "tcp://");
    let mut stream = TcpStream::connect(&tcp_addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    writeln!(stream, r#"{{"message":"boot","args":[]}}"#).unwrap();
    let mut resp_line = String::new();
    reader.read_line(&mut resp_line).unwrap();
    let parsed: Value = serde_json::from_str(&resp_line).unwrap();
    assert_eq!(parsed["reply"], true);

    writeln!(
        stream,
        r#"{{"message":"dispatch","args":[{{"session_id":100,"prompt":"over_tcp"}}]}}"#
    )
    .unwrap();
    resp_line.clear();
    reader.read_line(&mut resp_line).unwrap();
    let parsed: Value = serde_json::from_str(&resp_line).unwrap();
    assert_eq!(parsed["reply"], true);

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        writeln!(stream, r#"{{"message":"status"}}"#).unwrap();
        resp_line.clear();
        reader.read_line(&mut resp_line).unwrap();
        let st: Value = serde_json::from_str(&resp_line).unwrap();
        if st["reply"]["last_reply"]["string"] == "s100#1:over_tcp" {
            break;
        }
        assert!(Instant::now() < deadline, "TCP status timeout: {st}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn read_listening_addr(child: &mut Child, prefix: &str) -> String {
    let stdout = child.stdout.as_mut().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let val: Value = serde_json::from_str(&line)
        .unwrap_or_else(|e| panic!("expected listening JSON line, got {line:?}: {e}"));
    let url = val["listening"].as_str().unwrap();
    url.strip_prefix(prefix).unwrap().to_string()
}

fn http_request(addr: &str, method: &str, path: &str, body: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, json_body) = raw.split_once("\r\n\r\n").unwrap();
    let status_line = head.lines().next().unwrap();
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let val: Value = serde_json::from_str(json_body).unwrap();
    (code, val)
}

fn wait_for_http_status(addr: &str, predicate: impl Fn(&Value) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let (code, resp) = http_request(addr, "POST", "/status", "");
        assert_eq!(code, 200);
        if predicate(&resp["reply"]) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "status predicate timed out: {resp}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
