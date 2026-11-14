use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::json;
use wings_abi::ValueType;
use wings_vm::{
    IngressSender, IngressWake, RequestOutcome, Reservation, ResourceAccount, Resources,
    ScalarModule, ScalarWorker,
};

const THREAD_BYTES: usize = 256 * 1024;
const IO_TIMEOUT: Duration = Duration::from_millis(100);
const SOCKET_BUFFER_BYTES: libc::c_int = 16 * 1024;
const SOCKET_BUFFER_CEILING: libc::c_int = 64 * 1024;

#[derive(Default)]
struct Report {
    connections: u64,
    published: u64,
    replies: u64,
    rejected: u64,
    timed_out: u64,
    failed: u64,
    invalid: u64,
    io_errors: u64,
    wake_errors: u64,
    max_socket_buffer: u64,
}

struct Adapter {
    thread: Option<JoinHandle<Result<Report, String>>>,
    stopped: Arc<AtomicBool>,
    address: SocketAddr,
    _charge: Reservation,
}

impl Adapter {
    fn start(
        account: &ResourceAccount,
        sender: IngressSender,
        connections: usize,
        frames: usize,
    ) -> Result<Self, String> {
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .map_err(|_| "cannot query adapter page size")?;
        if !page.is_power_of_two() {
            return Err("unsupported adapter page size".into());
        }
        let charge = account.reserve(Resources {
            bytes: (THREAD_BYTES + 2 * page + 512 * 1024) as u64,
            ..Resources::default()
        })?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| error.to_string())?;
        configure_socket(&listener).map_err(|error| error.to_string())?;
        if unsafe { libc::listen(listener.as_raw_fd(), 1) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let stopped = Arc::new(AtomicBool::new(false));
        let signal = stopped.clone();
        let account = account.clone();
        let thread = thread::Builder::new()
            .name("wings-tcp-example".into())
            .stack_size(THREAD_BYTES)
            .spawn(move || serve(listener, sender, &account, connections, frames, &signal))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            thread: Some(thread),
            stopped,
            address,
            _charge: charge,
        })
    }

    fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn join(&mut self) -> Result<Report, String> {
        self.thread
            .take()
            .ok_or("adapter was already joined")?
            .join()
            .map_err(|_| "TCP adapter panicked")?
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    listener: TcpListener,
    sender: IngressSender,
    account: &ResourceAccount,
    max_connections: usize,
    frames: usize,
    stopped: &AtomicBool,
) -> Result<Report, String> {
    let mut report = Report::default();
    for _ in 0..max_connections {
        let _connection_charge = account.reserve(Resources {
            bytes: 256 * 1024,
            ..Resources::default()
        })?;
        let end = Instant::now() + Duration::from_secs(2);
        let stream = loop {
            if stopped.load(Ordering::SeqCst) || Instant::now() >= end {
                return Ok(report);
            }
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let mut descriptor = libc::pollfd {
                        fd: listener.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    let result = unsafe { libc::poll(&mut descriptor, 1, 20) };
                    if result < 0
                        && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                    {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.to_string()),
            }
        };
        report.connections += 1;
        if let Err(error) = handle_connection(stream, &sender, frames, stopped, &mut report) {
            if error.kind() != std::io::ErrorKind::UnexpectedEof {
                report.io_errors += 1;
            }
        }
    }
    Ok(report)
}

fn handle_connection(
    mut stream: TcpStream,
    sender: &IngressSender,
    max_frames: usize,
    stopped: &AtomicBool,
    report: &mut Report,
) -> std::io::Result<()> {
    report.max_socket_buffer = report.max_socket_buffer.max(configure_socket(&stream)?);
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    for _ in 0..max_frames {
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        let mut frame = [0; 32];
        read_frame(&mut stream, &mut frame)?;
        let sequence = u64::from_be_bytes(frame[..8].try_into().unwrap());
        let operation = u64::from_be_bytes(frame[8..16].try_into().unwrap());
        let value = i64::from_be_bytes(frame[16..24].try_into().unwrap());
        let reserved = u64::from_be_bytes(frame[24..].try_into().unwrap());
        if sequence == 0 || !matches!(operation, 1 | 2) || reserved != 0 {
            report.invalid += 1;
            break;
        }
        let outcome = match sender.request(
            &[value, i64::from(operation == 2)],
            Duration::from_millis(50),
        ) {
            Ok((mut request, wake)) => {
                report.published += 1;
                report.wake_errors += u64::from(wake == IngressWake::WakeFailed);
                request
                    .wait(Duration::from_millis(50))
                    .map_err(std::io::Error::other)?
                    .unwrap_or(RequestOutcome::TimedOut)
            }
            Err(_) => {
                report.rejected += 1;
                frame[8..16].copy_from_slice(&4u64.to_be_bytes());
                frame[16..24].copy_from_slice(&4i64.to_be_bytes());
                write_frame(&mut stream, &frame)?;
                continue;
            }
        };
        let (operation, value) = match outcome {
            RequestOutcome::Reply(value) => {
                report.replies += 1;
                (3u64, value)
            }
            RequestOutcome::TimedOut => {
                report.timed_out += 1;
                (4, 1)
            }
            RequestOutcome::Cancelled => {
                report.failed += 1;
                (4, 2)
            }
            RequestOutcome::Failed => {
                report.failed += 1;
                (4, 3)
            }
        };
        frame[8..16].copy_from_slice(&operation.to_be_bytes());
        frame[16..24].copy_from_slice(&value.to_be_bytes());
        write_frame(&mut stream, &frame)?;
    }
    Ok(())
}

fn configure_socket(socket: &impl AsRawFd) -> std::io::Result<u64> {
    let mut maximum = 0;
    for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        let result = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                (&SOCKET_BUFFER_BYTES as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut actual: libc::c_int = 0;
        let mut length = std::mem::size_of_val(&actual) as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                (&mut actual as *mut libc::c_int).cast(),
                &mut length,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if length as usize != std::mem::size_of::<libc::c_int>()
            || !(1..=SOCKET_BUFFER_CEILING).contains(&actual)
        {
            return Err(std::io::Error::other("socket buffer exceeds adapter limit"));
        }
        maximum = maximum.max(actual as u64);
    }
    Ok(maximum)
}

fn read_frame(stream: &mut TcpStream, frame: &mut [u8; 32]) -> std::io::Result<()> {
    let end = Instant::now() + IO_TIMEOUT;
    let mut offset = 0;
    while offset < frame.len() {
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(&mut frame[offset..]) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_frame(stream: &mut TcpStream, frame: &[u8; 32]) -> std::io::Result<()> {
    let end = Instant::now() + IO_TIMEOUT;
    let mut offset = 0;
    while offset < frame.len() {
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(&frame[offset..]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn run(directory: &Path, connections: usize, frames: usize) -> Result<(), String> {
    let limits = Resources {
        bytes: 32 << 20,
        processes: 1,
        messages: 4,
        requests: 4,
    };
    let vm = ResourceAccount::vm_accounted(limits)?;
    let bootstrap = vm.used();
    let tenant = vm.tenant_accounted(limits)?;
    let module = unsafe { ScalarModule::load_accounted(directory, &vm) }?;
    let handler = module.handler("Apply")?;
    if module.manifest().state != ValueType::Int
        || handler.schema().reply != Some(ValueType::Int)
        || handler.schema().parameters.len() != 2
        || handler.schema().parameters[0].1 != ValueType::Int
        || handler.schema().parameters[1].1 != ValueType::Bool
    {
        return Err("TCP Counter requires Int state and Apply(Int, Bool) -> Int".into());
    }
    let mut worker = ScalarWorker::new(&tenant, 1)?;
    worker.enable_wait()?;
    let pid = worker.spawn(&module, limits, 1)?;
    let sender = worker.open_request_ingress(pid, &handler, 2, 4096)?;
    drop(handler);
    let mut adapter = Adapter::start(&tenant, sender, connections, frames)?;
    writeln!(
        std::io::stdout(),
        "{}",
        json!({"listening": adapter.address.to_string()})
    )
    .and_then(|()| std::io::stdout().flush())
    .map_err(|error| error.to_string())?;
    while !adapter.is_finished() {
        worker.tick(64)?;
        worker.wait(Duration::from_millis(20))?;
    }
    let report = adapter.join()?;
    let committed = worker.committed_state(pid).ok().or_else(|| {
        worker
            .last_exit()
            .and_then(|snapshot| snapshot.committed_state)
    });
    drop((adapter, worker, module, tenant));
    if vm.used() != bootstrap {
        return Err("TCP service leaked resource reservations".into());
    }
    writeln!(
        std::io::stdout(),
        "{}",
        json!({
            "connections": report.connections, "published": report.published, "replies": report.replies,
            "rejected": report.rejected, "timed_out": report.timed_out, "failed": report.failed,
            "invalid": report.invalid, "io_errors": report.io_errors, "wake_errors": report.wake_errors,
            "max_socket_buffer": report.max_socket_buffer,
            "committed": committed, "resources_released": true,
            "bootstrap_bytes": bootstrap.bytes, "remaining_bytes": vm.used().bytes,
        })
    ).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_and_accepted_socket_buffers_have_verified_limits() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        assert!((1..=SOCKET_BUFFER_CEILING as u64).contains(&configure_socket(&listener).unwrap()));
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        assert!((1..=SOCKET_BUFFER_CEILING as u64).contains(&configure_socket(&server).unwrap()));
        drop((client, server, listener));
    }

    #[test]
    fn a_partial_frame_cannot_extend_the_read_deadline() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        let started = Instant::now();
        let mut client = client;
        client.write_all(&[1, 2, 3]).unwrap();
        let error = read_frame(&mut server, &mut [0; 32]).unwrap_err();
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ));
        assert!(started.elapsed() >= IO_TIMEOUT);
        drop((client, server, listener));
    }
}

fn main() -> std::process::ExitCode {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let result = (|| {
        if arguments.len() != 4 || arguments[0] != "--trusted" {
            return Err(
                "Usage: tcp_counter --trusted <artifact> <connections:1..=8> <frames:1..=64>"
                    .into(),
            );
        }
        let connections = arguments[2]
            .parse::<usize>()
            .map_err(|_| "invalid connection limit")?;
        let frames = arguments[3]
            .parse::<usize>()
            .map_err(|_| "invalid frame limit")?;
        if !(1..=8).contains(&connections) || !(1..=64).contains(&frames) {
            return Err("TCP adapter limits are out of range".into());
        }
        run(Path::new(&arguments[1]), connections, frames)
    })();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tcp_counter: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
