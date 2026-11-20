use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const TRACE_DIRECTORY: &str = "WINGS_STARTUP_TRACE_DIR";
const MAX_EVENTS: usize = 64;
const MAX_FILE_BYTES: usize = 8192;
const MAX_STAGE_BYTES: usize = 40;
const RECORD_BYTES: usize = 128;
const MAX_BODY_BYTES: usize = MAX_FILE_BYTES - RECORD_BYTES;

static TRACE: OnceLock<Option<StartupTrace>> = OnceLock::new();
static NEXT_LOAD: AtomicU64 = AtomicU64::new(1);

fn global() -> Option<&'static StartupTrace> {
    TRACE
        .get_or_init(|| {
            let directory = std::env::var_os(TRACE_DIRECTORY)?;
            if directory.is_empty() {
                return None;
            }
            StartupTrace::open(Path::new(&directory)).ok()
        })
        .as_ref()
}

/// Starts one uniquely identified module load in the opt-in diagnostic file.
/// Returns zero when tracing is disabled.
#[doc(hidden)]
pub fn begin_load() -> u64 {
    if global().is_some() {
        NEXT_LOAD.fetch_add(1, Ordering::Relaxed)
    } else {
        0
    }
}

/// Records a trusted startup phase in an opt-in diagnostic file.
///
/// This is intentionally hidden from the supported API. It never writes to
/// business stdout/stderr and disables itself on any diagnostic error.
#[doc(hidden)]
pub fn mark(load: u64, stage: &'static str) {
    if load == 0 {
        return;
    }
    if let Some(trace) = TRACE.get().and_then(Option::as_ref) {
        trace.mark(load, stage);
    }
}

struct StartupTrace {
    started: Instant,
    state: Mutex<TraceState>,
}

struct TraceState {
    file: Option<File>,
    events: usize,
    bytes: usize,
}

impl StartupTrace {
    fn open(directory: &Path) -> io::Result<Self> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| io::Error::other(error.to_string()))?
            .as_nanos();
        let path = directory.join(format!("startup-{}-{unique}.log", std::process::id()));
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self {
            started: Instant::now(),
            state: Mutex::new(TraceState {
                file: Some(file),
                events: 0,
                bytes: 0,
            }),
        })
    }

    fn mark(&self, load: u64, stage: &str) {
        if stage.is_empty()
            || stage.len() > MAX_STAGE_BYTES
            || !stage
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.file.is_none() {
            return;
        }
        if state.events >= MAX_EVENTS {
            state.finish(self.started);
            return;
        }
        let elapsed = self.started.elapsed().as_nanos();
        let Some(record) = Record::new(load, elapsed, stage) else {
            state.file = None;
            return;
        };
        if state.bytes + record.len > MAX_BODY_BYTES {
            state.finish(self.started);
            return;
        }
        state.write(record.as_bytes());
        state.events += 1;
    }
}

impl TraceState {
    fn write(&mut self, record: &[u8]) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if file.write_all(record).is_err() {
            self.file = None;
            return;
        }
        self.bytes += record.len();
    }

    fn finish(&mut self, started: Instant) {
        if let Some(record) =
            Record::new(0, started.elapsed().as_nanos(), "startup_trace_truncated")
        {
            self.write(record.as_bytes());
        }
        self.file = None;
    }
}

struct Record {
    bytes: [u8; RECORD_BYTES],
    len: usize,
}

impl Record {
    fn new(load: u64, elapsed: u128, stage: &str) -> Option<Self> {
        let mut bytes = [0; RECORD_BYTES];
        let len = {
            let mut cursor = &mut bytes[..];
            writeln!(cursor, "{} {load} {elapsed} {stage}", std::process::id()).ok()?;
            RECORD_BYTES - cursor.len()
        };
        Some(Self { bytes, len })
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[cfg(test)]
fn trace_path(directory: &Path) -> PathBuf {
    let mut entries = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    entries.pop().unwrap()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    fn directory(label: &str) -> PathBuf {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "wings-startup-trace-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        directory
    }

    #[test]
    fn records_ordered_bounded_process_stages() {
        let directory = directory("ordered");
        let trace = StartupTrace::open(&directory).unwrap();
        trace.mark(7, "module_load_begin");
        trace.mark(7, "module_dlopen_begin");
        trace.mark(7, "module_symbols_ready");
        trace.mark(7, "not-valid");
        trace.mark(7, "");

        let path = trace_path(&directory);
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3);
        let mut previous = 0;
        for (line, expected) in lines.iter().zip([
            "module_load_begin",
            "module_dlopen_begin",
            "module_symbols_ready",
        ]) {
            let fields = line.split(' ').collect::<Vec<_>>();
            assert_eq!(fields.len(), 4);
            assert_eq!(fields[0], std::process::id().to_string());
            assert_eq!(fields[1], "7");
            let elapsed = fields[2].parse::<u128>().unwrap();
            assert!(elapsed >= previous);
            previous = elapsed;
            assert_eq!(fields[3], expected);
        }
        assert!(contents.len() <= MAX_BODY_BYTES);

        drop(trace);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn explicitly_truncates_and_disables_after_fixed_events() {
        let directory = directory("truncated");
        let trace = StartupTrace::open(&directory).unwrap();
        for _ in 0..=MAX_EVENTS {
            trace.mark(7, "module_load_begin");
        }
        trace.mark(7, "module_symbols_ready");

        let path = trace_path(&directory);
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() <= MAX_FILE_BYTES);
        let contents = std::str::from_utf8(&bytes).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), MAX_EVENTS + 1);
        assert!(
            lines[..MAX_EVENTS]
                .iter()
                .all(|line| line.ends_with(" module_load_begin"))
        );
        assert!(lines[MAX_EVENTS].ends_with(" startup_trace_truncated"));

        drop(trace);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn concurrent_marks_preserve_load_identity() {
        let directory = directory("concurrent");
        let trace = std::sync::Arc::new(StartupTrace::open(&directory).unwrap());
        std::thread::scope(|scope| {
            for load in [11, 22] {
                let trace = trace.clone();
                scope.spawn(move || {
                    trace.mark(load, "module_load_begin");
                    trace.mark(load, "module_symbols_ready");
                });
            }
        });

        let path = trace_path(&directory);
        let contents = std::fs::read_to_string(&path).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 4);
        for load in ["11", "22"] {
            let mut stages = lines.iter().filter_map(|line| {
                let fields = line.split(' ').collect::<Vec<_>>();
                assert_eq!(fields.len(), 4);
                (fields[1] == load).then_some(fields[3])
            });
            assert_eq!(stages.next(), Some("module_load_begin"));
            assert_eq!(stages.next(), Some("module_symbols_ready"));
            assert_eq!(stages.next(), None);
        }

        drop(trace);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn missing_output_directory_disables_initialization() {
        let directory = directory("missing");
        let missing = directory.join("missing");
        assert!(StartupTrace::open(&missing).is_err());
        assert!(!missing.exists());
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn write_errors_disable_tracing_without_panicking() {
        let directory = directory("write-error");
        let path = directory.join("read-only.log");
        File::create(&path).unwrap();
        let file = OpenOptions::new().read(true).open(&path).unwrap();
        let trace = StartupTrace {
            started: Instant::now(),
            state: Mutex::new(TraceState {
                file: Some(file),
                events: 0,
                bytes: 0,
            }),
        };
        trace.mark(7, "module_load_begin");
        assert!(trace.state.lock().unwrap().file.is_none());
        trace.mark(7, "module_symbols_ready");
        assert_eq!(trace.state.lock().unwrap().bytes, 0);
        drop(trace);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
