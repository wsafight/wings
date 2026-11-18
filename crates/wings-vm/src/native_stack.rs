const INVALID: &str = "native thread stack range is invalid";

struct StackRange {
    start: usize,
    bytes: usize,
}

impl StackRange {
    fn validate(&self, maximum: usize, marker: usize) -> Result<(), &'static str> {
        let end = self.start.checked_add(self.bytes).ok_or(INVALID)?;
        if self.start == 0 || self.bytes == 0 || !(self.start..end).contains(&marker) {
            return Err(INVALID);
        }
        if self.bytes > maximum {
            return Err("native thread stack exceeds its reserved usable size");
        }
        Ok(())
    }
}

pub(crate) fn check_current(maximum: usize) -> Result<(), &'static str> {
    let range = current()?;
    range.validate(maximum, std::ptr::from_ref(&range) as usize)
}

#[cfg(target_os = "macos")]
fn current() -> Result<StackRange, &'static str> {
    let thread = unsafe { libc::pthread_self() };
    let end = unsafe { libc::pthread_get_stackaddr_np(thread) } as usize;
    let bytes = unsafe { libc::pthread_get_stacksize_np(thread) };
    Ok(StackRange {
        start: end.checked_sub(bytes).ok_or(INVALID)?,
        bytes,
    })
}

#[cfg(target_os = "linux")]
fn current() -> Result<StackRange, &'static str> {
    let mut attributes = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
    if unsafe { libc::pthread_getattr_np(libc::pthread_self(), attributes.as_mut_ptr()) } != 0 {
        return Err("cannot query native thread stack attributes");
    }
    let mut attributes = unsafe { attributes.assume_init() };
    let mut address = std::ptr::null_mut();
    let mut bytes = 0;
    let queried = unsafe { libc::pthread_attr_getstack(&attributes, &mut address, &mut bytes) };
    let destroyed = unsafe { libc::pthread_attr_destroy(&mut attributes) };
    if queried != 0 || destroyed != 0 {
        return Err("cannot read or release native thread stack attributes");
    }
    Ok(StackRange {
        start: address as usize,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_range_requires_nonzero_checked_bounds_and_a_local_marker() {
        let range = StackRange {
            start: 16,
            bytes: 32,
        };
        for marker in [16, 17, 47] {
            assert_eq!(range.validate(32, marker), Ok(()));
            assert_eq!(range.validate(64, marker), Ok(()));
        }
        for marker in [0, 15, 48, usize::MAX] {
            assert!(range.validate(32, marker).is_err());
        }
        for (start, bytes) in [(0, 32), (16, 0), (usize::MAX, 1), (16, usize::MAX)] {
            assert!(
                StackRange { start, bytes }
                    .validate(usize::MAX, 16)
                    .is_err()
            );
        }
    }

    #[test]
    fn larger_than_reserved_usable_stack_is_not_silently_accepted() {
        let range = StackRange {
            start: 16,
            bytes: 32,
        };
        for maximum in [0, 1, 31] {
            assert_eq!(
                range.validate(maximum, 17),
                Err("native thread stack exceeds its reserved usable size")
            );
        }
    }

    #[test]
    fn native_explicit_worker_and_timer_threads_report_their_real_usable_bounds() {
        for requested in [256 << 10, 2 << 20] {
            std::thread::Builder::new()
                .stack_size(requested)
                .spawn(move || {
                    let range = current().unwrap();
                    println!("requested={requested}, reported_usable={}", range.bytes);
                    assert!(range.bytes > 0);
                    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
                    let original_stack_charge = requested
                        + if requested == 256 << 10 {
                            2 * page
                        } else {
                            65536
                        };
                    check_current(original_stack_charge).unwrap();
                    check_current(range.bytes).unwrap();
                    assert!(check_current(range.bytes - 1).is_err());
                    assert!(check_current(0).is_err());
                })
                .unwrap()
                .join()
                .unwrap();
        }
    }
}
