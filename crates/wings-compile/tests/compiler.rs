use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use wings_compile::{build, build_with_frame_limit, check, render_diagnostics};
use wings_source::{Diagnostic, Span};

const SOURCE: &str =
    "actor Counter { state count = 0; on inc(by: int) -> int { count += by; count } }";
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "wings-compile-test-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn source(&self, source: &str) -> PathBuf {
        let path = self.0.join("input source.wings");
        fs::write(&path, source).unwrap();
        path
    }

    fn artifact(&self) -> PathBuf {
        self.0.join("artifact")
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn cli(arguments: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wings-compile"))
        .args(arguments)
        .output()
        .unwrap()
}

fn assert_exit(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn check_accepts_the_repository_counter_without_diagnostics() {
    let source = include_str!("../../../examples/counter.wings");
    let analysis = check(source);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(analysis.parsed.program.actors[0].handlers.len(), 5);
    assert!(render_diagnostics("counter.wings", source, &analysis).is_empty());
}

#[test]
fn rendered_diagnostics_include_one_based_utf16_positions_codes_and_paths() {
    let source = "// 😀é\nactor P { state count: int = false }";
    let mut analysis = check(source);
    analysis.diagnostics = vec![
        Diagnostic::error(
            "TEST",
            "after emoji",
            Span::new("// 😀".len(), "// 😀".len()),
        ),
        Diagnostic::error(
            "NEXT",
            "second line",
            Span::new(source.find("actor").unwrap(), source.find("actor").unwrap()),
        ),
    ];
    assert_eq!(
        render_diagnostics("dir/input.wings", source, &analysis),
        "dir/input.wings:1:6: error[TEST]: after emoji\ndir/input.wings:2:1: error[NEXT]: second line\n"
    );
}

#[test]
fn build_does_not_emit_files_when_frontend_reports_errors() {
    let directory = Directory::new();
    let too_many_states = "actor P { state s1 = 0; state s2 = 0; state s3 = 0; state s4 = 0; state s5 = 0; state s6 = 0; state s7 = 0; state s8 = 0; state s9 = 0 }";
    let error = build(too_many_states, &directory.artifact()).unwrap_err();
    assert!(error.contains("error[T001]"), "{error}");
    assert!(!directory.artifact().exists());
}

#[test]
fn public_build_reports_lowering_only_rejections() {
    let directory = Directory::new();
    for source in [
        "",
        "actor A { state count = 0 } actor B { state count = 0 }",
    ] {
        assert!(check(source).diagnostics.is_empty());
        assert!(
            build(source, &directory.artifact())
                .unwrap_err()
                .contains("exactly one actor")
        );
        assert!(!directory.artifact().exists());
    }
}

#[test]
fn public_build_propagates_backend_frame_limit_rejections() {
    let directory = Directory::new();
    for limit in [0, wings_codegen::MAX_SCALAR_FRAME_BYTES + 1, u32::MAX] {
        assert!(
            build_with_frame_limit(SOURCE, &directory.artifact(), limit)
                .unwrap_err()
                .contains("frame limit must be within")
        );
        assert!(!directory.artifact().exists());
    }
}

#[test]
fn cli_help_succeeds_without_reading_source() {
    for option in ["--help", "-h"] {
        let output = cli(&[OsStr::new(option)]);
        assert_exit(&output, 0);
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: wings-compile"));
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn cli_rejects_invalid_argument_shapes_with_exit_code_two() {
    for arguments in [
        &[][..],
        &[OsStr::new("check")],
        &[OsStr::new("check"), OsStr::new("a"), OsStr::new("b")],
        &[OsStr::new("build"), OsStr::new("a")],
        &[
            OsStr::new("build"),
            OsStr::new("a"),
            OsStr::new("--out"),
            OsStr::new("b"),
        ],
        &[OsStr::new("--help"), OsStr::new("extra")],
    ] {
        let output = cli(arguments);
        assert_exit(&output, 2);
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Usage: wings-compile"));
    }
}

#[test]
fn cli_check_accepts_valid_source_and_reports_diagnostics_with_exit_code_one() {
    let directory = Directory::new();
    let valid = directory.source(SOURCE);
    let output = cli(&[OsStr::new("check"), valid.as_os_str()]);
    assert_exit(&output, 0);
    assert!(String::from_utf8_lossy(&output.stdout).contains("check passed"));
    assert!(output.stderr.is_empty());

    let invalid = directory.source("actor P { state count: int = false }");
    let output = cli(&[OsStr::new("check"), invalid.as_os_str()]);
    assert_exit(&output, 1);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error[T005]"));
}

#[test]
fn cli_reports_missing_and_non_utf8_files_with_exit_code_two() {
    let directory = Directory::new();
    let missing = directory.0.join("missing.wings");
    let output = cli(&[OsStr::new("check"), missing.as_os_str()]);
    assert_exit(&output, 2);
    assert!(output.stdout.is_empty());

    let invalid_utf8 = directory.0.join("invalid.wings");
    fs::write(&invalid_utf8, [0xff]).unwrap();
    assert_exit(&cli(&[OsStr::new("check"), invalid_utf8.as_os_str()]), 2);
}

#[test]
fn cli_rejects_sources_above_the_frontend_size_limit() {
    let directory = Directory::new();
    let source = directory.source(&" ".repeat(1024 * 1024 + 1));
    let output = cli(&[OsStr::new("check"), source.as_os_str()]);
    assert_exit(&output, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("1 MiB frontend limit"));
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
))]
#[test]
fn cli_build_emits_a_complete_artifact_and_refuses_to_replace_it() {
    let directory = Directory::new();
    let source = directory.source(SOURCE);
    let artifact = directory.artifact();
    let arguments = [
        OsStr::new("build"),
        source.as_os_str(),
        OsStr::new("-o"),
        artifact.as_os_str(),
    ];
    let output = cli(&arguments);
    assert_exit(&output, 0);
    assert!(String::from_utf8_lossy(&output.stdout).contains("scalar AOT artifact built"));
    let manifest = fs::read(artifact.join("manifest.json")).unwrap();
    assert!(!manifest.is_empty());
    assert!(!artifact.join("program.o").exists());
    let output = cli(&arguments);
    assert_exit(&output, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("create new artifact directory"));
    assert_eq!(fs::read(artifact.join("manifest.json")).unwrap(), manifest);
}

#[test]
fn cli_build_reports_lowering_errors_without_creating_output() {
    let directory = Directory::new();
    let source = directory.source("actor A { state count = 0 } actor B { state count = 0 }");
    let artifact = directory.artifact();
    let output = cli(&[
        OsStr::new("build"),
        source.as_os_str(),
        OsStr::new("-o"),
        artifact.as_os_str(),
    ]);
    assert_exit(&output, 1);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("build failed: scalar AOT requires exactly one actor")
    );
    assert!(!artifact.exists());
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
))]
#[test]
fn cli_missing_linker_leaves_no_manifest() {
    let directory = Directory::new();
    let source = directory.source(SOURCE);
    let artifact = directory.artifact();
    let output = Command::new(env!("CARGO_BIN_EXE_wings-compile"))
        .args([
            OsStr::new("build"),
            source.as_os_str(),
            OsStr::new("-o"),
            artifact.as_os_str(),
        ])
        .env("CC", directory.0.join("missing-linker"))
        .output()
        .unwrap();
    assert_exit(&output, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("start system linker (cc or CC)"));
    assert!(artifact.join("program.o").is_file());
    assert!(!artifact.join("manifest.json").exists());
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
))]
#[test]
fn cli_nonzero_linker_exit_leaves_no_manifest() {
    let directory = Directory::new();
    let source = directory.source(SOURCE);
    let artifact = directory.artifact();
    let output = Command::new(env!("CARGO_BIN_EXE_wings-compile"))
        .args([
            OsStr::new("build"),
            source.as_os_str(),
            OsStr::new("-o"),
            artifact.as_os_str(),
        ])
        .env("CC", "/usr/bin/false")
        .output()
        .unwrap();
    assert_exit(&output, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("system linker failed:"));
    assert!(artifact.join("program.o").is_file());
    assert!(!artifact.join("manifest.json").exists());
}

#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
))]
#[test]
fn v2_ergonomic_features_build_cleanly() {
    let directory = Directory::new();
    let showcase = r#"
    actor Showcase {
      struct Meta { tag: str, score: int }
      struct Card { meta: Meta, active: bool }

      state total = 0
      state label = "boot"
      state card = Card { meta: Meta { tag: "v2", score: 10 }, active: true }
      state history: [int] = []

      fn add_scaled(base: int, factor: int) -> int => base + factor * 2

      fn sum_list(xs: [int]) -> int {
        let acc = 0
        for item in xs {
          acc += item
        }
        acc
      }

      on run_all(limit: int) -> str {
        for i in 1..=limit {
          if i % 2 == 0 {
            total += i
          } else if i == 3 {
            history = [i, ..history]
          }
        }
        let piped = total |> add_scaled(5)
        card.meta.score += piped
        let updated = Card { active: !card.active, ..card }
        card = updated
        let head_bonus = match history {
          [] => 0,
          [h, ..rest] => h + sum_list(rest),
        }
        let raw = b"ok\x21"
        let ok = (false && (1 / 0 == 0)) || (true || (1 / 0 == 1))
        label = f"{card.meta.tag}:{card.meta.score + head_bonus}:{ok}:{raw.len()}"
        label
      }
    }
    "#;
    build(showcase, &directory.artifact()).unwrap();
    assert!(directory.artifact().join("manifest.json").is_file());
}
