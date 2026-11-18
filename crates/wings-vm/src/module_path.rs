use std::ffi::{CStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

const MAX_DIRECTORY_BYTES: usize = 16 << 10;
const NATIVE_PATH_BYTES: usize = libc::PATH_MAX as usize;

pub(super) fn resolve(directory: &Path) -> Result<PathBuf, String> {
    resolve_with(directory, |source, output| {
        let pointer = unsafe { libc::realpath(source.as_ptr(), output.as_mut_ptr().cast()) };
        if pointer.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if pointer != output.as_mut_ptr().cast() {
            return Err("native module path resolver returned an unexpected buffer".into());
        }
        Ok(())
    })
}

fn resolve_with(
    directory: &Path,
    resolver: impl FnOnce(&CStr, &mut [u8; NATIVE_PATH_BYTES]) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let bytes = directory.as_os_str().as_bytes();
    if bytes.len() > MAX_DIRECTORY_BYTES {
        return Err("artifact directory path exceeds 16 KiB".into());
    }
    if bytes.contains(&0) {
        return Err("artifact directory path contains NUL".into());
    }
    let mut source = [0; MAX_DIRECTORY_BYTES + 1];
    source[..bytes.len()].copy_from_slice(bytes);
    let source = CStr::from_bytes_with_nul(&source[..=bytes.len()])
        .map_err(|_| "invalid native module path source")?;
    let mut output = [u8::MAX; NATIVE_PATH_BYTES];
    resolver(source, &mut output)?;
    let result = CStr::from_bytes_until_nul(&output)
        .map_err(|_| "native module path is not terminated")?
        .to_bytes();
    if result.first() != Some(&b'/') {
        return Err("native module path is not absolute".into());
    }
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(result.len())
        .map_err(|_| "module path storage allocation failed")?;
    if owned.capacity() != result.len() {
        return Err("allocator changed module path capacity".into());
    }
    owned.extend_from_slice(result);
    Ok(PathBuf::from(OsString::from_vec(owned)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::os::unix::fs::symlink;

    #[test]
    fn invalid_sources_are_rejected_before_resolving() {
        for bytes in [vec![b'a'; MAX_DIRECTORY_BYTES + 1], b"a\0b".to_vec()] {
            assert!(
                resolve_with(Path::new(&OsString::from_vec(bytes)), |_, _| {
                    panic!("resolved an invalid source")
                })
                .is_err()
            );
        }
    }

    #[test]
    fn exact_limit_empty_and_non_utf8_sources_use_the_fixed_buffers() {
        for bytes in [Vec::new(), vec![b'a'; MAX_DIRECTORY_BYTES], vec![0xff; 8]] {
            let path = PathBuf::from(OsString::from_vec(bytes.clone()));
            let called = Cell::new(false);
            let result = resolve_with(&path, |source, output| {
                called.set(true);
                assert_eq!(source.to_bytes(), bytes);
                assert_eq!(source.to_bytes_with_nul().len(), bytes.len() + 1);
                assert_eq!(output.len(), NATIVE_PATH_BYTES);
                output[..4].copy_from_slice(b"/ok\0");
                Ok(())
            })
            .unwrap();
            assert!(called.get());
            assert_eq!(result, Path::new("/ok"));
        }
    }

    #[test]
    fn output_requires_an_absolute_terminated_native_path() {
        assert!(resolve_with(Path::new("."), |_, _| Ok(())).is_err());
        for bytes in [b"\0".as_slice(), b"relative\0"] {
            assert!(
                resolve_with(Path::new("."), |_, output| {
                    output[..bytes.len()].copy_from_slice(bytes);
                    Ok(())
                })
                .is_err()
            );
        }
        assert!(
            resolve_with(Path::new("."), |_, _| Err("native failure".into()))
                .is_err_and(|error| error == "native failure")
        );
        let result = resolve_with(Path::new("."), |_, output| {
            output.fill(b'a');
            output[0] = b'/';
            output[NATIVE_PATH_BYTES - 1] = 0;
            Ok(())
        })
        .unwrap();
        assert_eq!(result.as_os_str().len(), NATIVE_PATH_BYTES - 1);
        let result = resolve_with(Path::new("."), |_, output| {
            output[..3].copy_from_slice(b"/\xff\0");
            Ok(())
        })
        .unwrap();
        assert_eq!(result.as_os_str().as_bytes(), b"/\xff");
    }

    #[test]
    fn fixed_storage_fits_the_unchanged_module_control_budget() {
        const {
            assert!(MAX_DIRECTORY_BYTES + 1 + 2 * NATIVE_PATH_BYTES < 2 << 20);
            assert!(NATIVE_PATH_BYTES > 1 && NATIVE_PATH_BYTES <= MAX_DIRECTORY_BYTES);
        }
    }

    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn native_resolution_matches_canonicalize_for_links_and_non_utf8_paths() {
        let root = std::env::temp_dir().join(format!("wings-module-path-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let fixture = Fixture(root);
        let directory = fixture.0.join("native-中文");
        std::fs::create_dir(&directory).unwrap();
        let link = fixture.0.join("link");
        symlink(&directory, &link).unwrap();
        for path in [
            Path::new("."),
            fixture.0.as_path(),
            &directory,
            &link,
            &link.join(".."),
        ] {
            assert_eq!(resolve(path).unwrap(), path.canonicalize().unwrap());
        }
        let loop_path = fixture.0.join("loop");
        symlink("loop", &loop_path).unwrap();
        for path in [Path::new(""), &fixture.0.join("missing"), &loop_path] {
            assert!(resolve(path).is_err());
            assert!(path.canonicalize().is_err());
        }
        let non_utf8 = fixture.0.join(OsString::from_vec(b"native-\xff".to_vec()));
        #[cfg(target_os = "linux")]
        std::fs::create_dir(&non_utf8).unwrap();
        match (resolve(&non_utf8), non_utf8.canonicalize()) {
            (Ok(actual), Ok(expected)) => assert_eq!(actual, expected),
            (Err(actual), Err(expected)) => assert_eq!(actual, expected.to_string()),
            (actual, expected) => panic!("native resolution differs: {actual:?}, {expected:?}"),
        }
    }
}
