#![cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use wings_abi::{SCALAR_ABI_VERSION, ValueType, host_target};
use wings_artifact::{
    FORMAT_VERSION, Handler, INITIALIZER_SYMBOL, MAX_MANIFEST_BYTES, Manifest, handler_symbol,
    library_name,
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "wings-artifact-test-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn read_json(&self, value: &Value) -> Result<Manifest, String> {
        fs::write(
            self.path().join("manifest.json"),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
        Manifest::read(self.path())
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.path());
    }
}

fn manifest() -> Manifest {
    Manifest {
        format: FORMAT_VERSION,
        abi: SCALAR_ABI_VERSION,
        target: host_target().unwrap().into(),
        process: "Counter".into(),
        state: ValueType::Int,
        handlers: vec![Handler {
            name: "Add".into(),
            parameters: vec![("amount".into(), ValueType::Int)],
            reply: Some(ValueType::Int),
        }],
    }
}

fn rejects(manifest: &Manifest, message: &str) {
    let error = manifest.validate().unwrap_err();
    assert!(error.contains(message), "unexpected error: {error}");
}

#[test]
fn encoded_manifest_round_trips_through_disk() {
    let directory = Directory::new();
    let manifest = manifest();
    let encoded = manifest.encode().unwrap();
    fs::write(directory.path().join("manifest.json"), &encoded).unwrap();
    let decoded = Manifest::read(directory.path()).unwrap();
    assert_eq!(decoded.encode().unwrap(), encoded);
}

#[test]
fn round_trip_preserves_all_value_types_and_one_way_vs_unit_reply() {
    let mut manifest = manifest();
    manifest.state = ValueType::Bool;
    manifest.handlers = vec![
        Handler {
            name: "Notify".into(),
            parameters: vec![],
            reply: None,
        },
        Handler {
            name: "Ack".into(),
            parameters: vec![
                ("flag".into(), ValueType::Bool),
                ("unit".into(), ValueType::Unit),
            ],
            reply: Some(ValueType::Unit),
        },
    ];
    let directory = Directory::new();
    let value: Value = serde_json::from_slice(&manifest.encode().unwrap()).unwrap();
    let decoded = directory.read_json(&value).unwrap();
    assert_eq!(decoded.state, ValueType::Bool);
    assert_eq!(decoded.handlers[0].reply, None);
    assert_eq!(decoded.handlers[1].reply, Some(ValueType::Unit));
    assert_eq!(
        decoded.handlers[1].parameters,
        manifest.handlers[1].parameters
    );
}

#[test]
fn rejects_unknown_format_and_old_or_future_scalar_abi_versions() {
    for version in [0, FORMAT_VERSION + 1, u32::MAX] {
        let mut manifest = manifest();
        manifest.format = version;
        rejects(&manifest, "unsupported artifact format");
        assert!(manifest.encode().is_err());
    }
    for version in [0, 1, 2, SCALAR_ABI_VERSION + 1, u32::MAX] {
        let mut manifest = manifest();
        manifest.abi = version;
        rejects(&manifest, "scalar ABI version");
    }
}

#[test]
fn rejects_foreign_or_unrecognized_targets() {
    for target in [
        "",
        "../program.so",
        "wasm32-unknown-unknown",
        "AARCH64-APPLE-DARWIN",
    ] {
        let mut manifest = manifest();
        manifest.target = target.into();
        rejects(&manifest, "target does not match");
    }
    let mut manifest = manifest();
    manifest.target = if host_target().unwrap() == "aarch64-apple-darwin" {
        "x86_64-unknown-linux-gnu"
    } else {
        "aarch64-apple-darwin"
    }
    .into();
    rejects(&manifest, "target does not match");
}

#[test]
fn accepts_empty_handler_list_and_exact_handler_limit() {
    let mut manifest = manifest();
    manifest.handlers.clear();
    manifest.validate().unwrap();
    manifest.handlers = (0..128)
        .map(|index| Handler {
            name: format!("H{index}"),
            parameters: vec![],
            reply: None,
        })
        .collect();
    manifest.validate().unwrap();
    manifest.handlers.push(Handler {
        name: "extra".into(),
        parameters: vec![],
        reply: None,
    });
    rejects(&manifest, "handler count");
}

#[test]
fn parameter_limit_is_inclusive() {
    let mut manifest = manifest();
    manifest.handlers[0].parameters = (0..64)
        .map(|index| (format!("p{index}"), ValueType::Int))
        .collect();
    manifest.validate().unwrap();
    manifest.handlers[0]
        .parameters
        .push(("extra".into(), ValueType::Int));
    rejects(&manifest, "too many parameters");
}

#[test]
fn rejects_duplicate_handler_names_but_allows_parameter_names_in_other_handlers() {
    let mut manifest = manifest();
    manifest.handlers.push(manifest.handlers[0].clone());
    rejects(&manifest, "duplicate handler name");
    manifest.handlers[1].name = "Other".into();
    manifest.validate().unwrap();
}

#[test]
fn rejects_duplicate_parameter_names_even_with_different_types() {
    let mut manifest = manifest();
    manifest.handlers[0]
        .parameters
        .push(("amount".into(), ValueType::Bool));
    rejects(&manifest, "duplicate parameter name");
}

#[test]
fn rejects_empty_long_and_control_character_names_at_each_level() {
    for name in [
        "".to_owned(),
        "x".repeat(257),
        "bad\nname".into(),
        "bad\0name".into(),
        "bad\u{7f}name".into(),
        "bad\u{85}name".into(),
    ] {
        let mut process = manifest();
        process.process = name.clone();
        rejects(&process, "process name");
        let mut handler = manifest();
        handler.handlers[0].name = name.clone();
        rejects(&handler, "handler name");
        let mut parameter = manifest();
        parameter.handlers[0].parameters[0].0 = name;
        rejects(&parameter, "parameter name");
    }
}

#[test]
fn name_length_limit_counts_utf8_bytes() {
    let mut manifest = manifest();
    manifest.process = "x".repeat(256);
    manifest.handlers[0].name = "é".repeat(128);
    manifest.handlers[0].parameters[0].0 = "值".repeat(85);
    manifest.validate().unwrap();
    manifest.handlers[0].name.push('é');
    rejects(&manifest, "handler name");
}

#[test]
fn rejects_unknown_fields_including_attempted_library_path_override() {
    let directory = Directory::new();
    for field in ["library", "library_path", "unexpected"] {
        let mut value = serde_json::to_value(manifest()).unwrap();
        value[field] = json!("../../outside.so");
        let error = directory.read_json(&value).unwrap_err();
        assert!(error.contains("unknown field"), "{error}");
    }
    let mut value = serde_json::to_value(manifest()).unwrap();
    value["handlers"][0]["symbol"] = json!("arbitrary_entry");
    assert!(
        directory
            .read_json(&value)
            .unwrap_err()
            .contains("unknown field")
    );
}

#[test]
fn rejects_malformed_schema_missing_fields_and_unknown_value_types() {
    let directory = Directory::new();
    for field in ["format", "abi", "target", "process", "state", "handlers"] {
        let mut value = serde_json::to_value(manifest()).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            directory
                .read_json(&value)
                .unwrap_err()
                .contains("missing field")
        );
    }
    for (field, value) in [
        ("state", json!("String")),
        ("handlers", json!({})),
        ("abi", json!(-1)),
        ("format", json!("1")),
    ] {
        let mut data = serde_json::to_value(manifest()).unwrap();
        data[field] = value;
        assert!(
            directory.read_json(&data).is_err(),
            "accepted malformed {field}"
        );
    }
    let mut value = serde_json::to_value(manifest()).unwrap();
    value["handlers"][0]["parameters"] = json!([["x", "Int", "extra"]]);
    assert!(directory.read_json(&value).is_err());
}

#[test]
fn read_applies_semantic_validation_after_json_parsing() {
    let directory = Directory::new();
    let mut value = serde_json::to_value(manifest()).unwrap();
    value["abi"] = json!(1);
    assert!(
        directory
            .read_json(&value)
            .unwrap_err()
            .contains("scalar ABI version")
    );
    value["abi"] = json!(SCALAR_ABI_VERSION);
    value["handlers"][0]["parameters"] = json!([["x", "Int"], ["x", "Bool"]]);
    assert!(
        directory
            .read_json(&value)
            .unwrap_err()
            .contains("duplicate parameter")
    );
}

#[test]
fn reader_accepts_exact_byte_limit_and_rejects_one_extra_byte() {
    let directory = Directory::new();
    let mut bytes = manifest().encode().unwrap();
    bytes.resize(MAX_MANIFEST_BYTES as usize, b' ');
    fs::write(directory.path().join("manifest.json"), &bytes).unwrap();
    Manifest::read(directory.path()).unwrap();
    bytes.push(b' ');
    fs::write(directory.path().join("manifest.json"), bytes).unwrap();
    assert!(
        Manifest::read(directory.path())
            .unwrap_err()
            .contains("exceeds 64 KiB")
    );
}

#[test]
fn encoding_rejects_structurally_valid_but_oversized_metadata() {
    let mut manifest = manifest();
    manifest.handlers = (0..128)
        .map(|index| Handler {
            name: format!("handler{index}"),
            parameters: (0..64)
                .map(|parameter| (format!("p{parameter}"), ValueType::Int))
                .collect(),
            reply: None,
        })
        .collect();
    manifest.validate().unwrap();
    assert!(manifest.encode().unwrap_err().contains("exceeds 64 KiB"));
}

#[test]
fn reader_rejects_missing_truncated_nonutf8_and_trailing_data() {
    let directory = Directory::new();
    assert!(Manifest::read(directory.path()).is_err());
    for bytes in [vec![], b"{".to_vec(), vec![0xff], b"null".to_vec()] {
        fs::write(directory.path().join("manifest.json"), bytes).unwrap();
        assert!(Manifest::read(directory.path()).is_err());
    }
    let mut bytes = manifest().encode().unwrap();
    bytes.extend_from_slice(b" {}");
    fs::write(directory.path().join("manifest.json"), bytes).unwrap();
    assert!(
        Manifest::read(directory.path())
            .unwrap_err()
            .contains("trailing")
    );
}

#[test]
fn rejects_duplicate_json_fields_in_manifest_and_handler() {
    let directory = Directory::new();
    let manifest = manifest();
    let bytes = String::from_utf8(manifest.encode().unwrap()).unwrap();
    for (old, new) in [
        ("\"format\": 1", "\"format\": 1, \"format\": 1"),
        (
            "\"name\": \"Add\"",
            "\"name\": \"Add\", \"name\": \"Other\"",
        ),
    ] {
        let duplicate = bytes.replacen(old, new, 1);
        assert_ne!(duplicate, bytes);
        fs::write(directory.path().join("manifest.json"), duplicate).unwrap();
        let error = Manifest::read(directory.path()).unwrap_err();
        assert!(error.contains("duplicate field"), "{error}");
    }
}

#[test]
fn reader_uses_only_manifest_json_under_the_requested_directory() {
    let directory = Directory::new();
    let nested = directory.path().join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(
        directory.path().join("manifest.json"),
        manifest().encode().unwrap(),
    )
    .unwrap();
    // It must not walk up to a parent manifest or accept an arbitrary filename.
    assert!(Manifest::read(&nested).is_err());
    fs::rename(
        directory.path().join("manifest.json"),
        nested.join("other.json"),
    )
    .unwrap();
    assert!(Manifest::read(&nested).is_err());
    assert!(Manifest::read(&nested.join("other.json")).is_err());
}

#[test]
fn library_and_symbols_are_fixed_independently_of_source_names() {
    assert_eq!(INITIALIZER_SYMBOL, "wings_scalar_init");
    assert_eq!(handler_symbol(0), "wings_scalar_handler_0");
    assert_eq!(handler_symbol(127), "wings_scalar_handler_127");
    assert_eq!(
        library_name(),
        if cfg!(target_os = "macos") {
            "program.dylib"
        } else {
            "program.so"
        }
    );
    assert_eq!(Path::new(library_name()).components().count(), 1);
}
