use crate::ResourceAccount;
use crate::allocator::{NativeAllocatorStats, global_allocator_stats};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeProcessMemory {
    pub resident_bytes: u64,
    pub footprint_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeMemoryLedger {
    pub vm_ledger_bytes: u64,
    pub allocator: NativeAllocatorStats,
    pub os_process: Option<NativeProcessMemory>,
}

pub fn native_memory_ledger(vm: &ResourceAccount) -> NativeMemoryLedger {
    NativeMemoryLedger {
        vm_ledger_bytes: vm.used().bytes,
        allocator: global_allocator_stats(),
        os_process: sample_process_memory(),
    }
}

#[cfg(target_os = "macos")]
pub fn sample_process_memory() -> Option<NativeProcessMemory> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage_info_v0>::zeroed();
    let rc = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V0,
            usage.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    Some(NativeProcessMemory {
        resident_bytes: usage.ri_resident_size,
        footprint_bytes: usage.ri_phys_footprint,
    })
}

#[cfg(target_os = "linux")]
pub fn sample_process_memory() -> Option<NativeProcessMemory> {
    let page_size = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    let resident_bytes = rss_pages.checked_mul(page_size)?;

    let footprint_bytes = std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|rollup| {
            for line in rollup.lines() {
                if let Some(rest) = line.strip_prefix("Pss:") {
                    let kib: u64 = rest.split_whitespace().next()?.parse().ok()?;
                    return kib.checked_mul(1024);
                }
            }
            None
        })
        .unwrap_or(resident_bytes);

    Some(NativeProcessMemory {
        resident_bytes,
        footprint_bytes,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn sample_process_memory() -> Option<NativeProcessMemory> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Resources;

    #[test]
    fn sample_process_memory_reports_positive_kernel_footprint() {
        let vm = ResourceAccount::vm_accounted(Resources {
            bytes: 1 << 20,
            processes: 1,
            messages: 1,
            requests: 1,
        })
        .unwrap();
        let ledger = native_memory_ledger(&vm);
        assert!(ledger.vm_ledger_bytes > 0);
        let os = ledger.os_process.expect("supported platform memory query");
        assert!(os.resident_bytes > 0);
        assert!(os.footprint_bytes > 0);
    }
}
