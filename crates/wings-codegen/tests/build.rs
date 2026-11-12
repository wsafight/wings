#![cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
))]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use wings_abi::ValueType;
use wings_artifact::{Manifest, library_name};
use wings_codegen::{MAX_SCALAR_FRAME_BYTES, build, build_with_frame_limit};
use wings_ir::{Function, Handler, Instruction, Operation, Operator, Program};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "wings-codegen-test-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
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

fn instruction(ty: ValueType, operation: Operation) -> Instruction {
    Instruction { ty, operation }
}

fn program() -> Program {
    Program {
        name: "Counter".into(),
        state: ValueType::Int,
        initializer: Function {
            instructions: vec![
                instruction(ValueType::Int, Operation::Constant(0)),
                instruction(ValueType::Unit, Operation::Constant(0)),
            ],
            state: 0,
            reply: 1,
        },
        handlers: vec![Handler {
            name: "Add".into(),
            parameters: vec![("delta".into(), ValueType::Int)],
            reply: Some(ValueType::Int),
            function: Function {
                instructions: vec![
                    instruction(ValueType::Int, Operation::State),
                    instruction(ValueType::Int, Operation::Argument(0)),
                    instruction(ValueType::Int, Operation::Binary(Operator::Add, 0, 1)),
                ],
                state: 2,
                reply: 2,
            },
        }],
    }
}

#[test]
fn builds_a_host_library_and_matching_manifest_without_leaving_object_files() {
    let directory = Directory::new();
    build(&program(), &directory.artifact()).unwrap();
    let manifest = Manifest::read(&directory.artifact()).unwrap();
    assert_eq!(manifest.process, "Counter");
    assert_eq!(manifest.state, ValueType::Int);
    assert_eq!(manifest.handlers.len(), 1);
    assert_eq!(manifest.handlers[0].name, "Add");
    assert_eq!(
        manifest.handlers[0].parameters,
        vec![("delta".into(), ValueType::Int)]
    );
    assert_eq!(manifest.handlers[0].reply, Some(ValueType::Int));
    assert!(
        fs::metadata(directory.artifact().join(library_name()))
            .unwrap()
            .len()
            > 0
    );
    assert!(!directory.artifact().join("program.o").exists());
    assert_eq!(fs::read_dir(directory.artifact()).unwrap().count(), 2);
}

#[test]
fn compiles_every_arithmetic_operation_including_runtime_fault_cases() {
    let directory = Directory::new();
    let mut program = program();
    let template = program.handlers[0].clone();
    program.handlers = [
        Operator::Add,
        Operator::Subtract,
        Operator::Multiply,
        Operator::Divide,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, operator)| {
        let mut handler = template.clone();
        handler.name = format!("Arithmetic{index}");
        handler.function.instructions[2].operation = Operation::Binary(operator, 0, 1);
        handler
    })
    .collect();
    let mut negate = template;
    negate.name = "Negate".into();
    negate.function.instructions[2].operation = Operation::Negate(1);
    program.handlers.push(negate);
    let mut fault = program.handlers[3].clone();
    fault.name = "DivisionByZero".into();
    fault.function.instructions[1].operation = Operation::Constant(0);
    program.handlers.push(fault);
    // Build only: arithmetic faults are not executed in the test host.
    build(&program, &directory.artifact()).unwrap();
    assert_eq!(
        Manifest::read(&directory.artifact())
            .unwrap()
            .handlers
            .len(),
        6
    );
}

#[test]
fn builds_boolean_and_unit_state_programs() {
    for ty in [ValueType::Bool, ValueType::Unit] {
        let directory = Directory::new();
        let mut program = program();
        program.state = ty;
        program.initializer.instructions[0].ty = ty;
        program.handlers[0].parameters[0].1 = ty;
        program.handlers[0].reply = Some(ty);
        program.handlers[0].function = Function {
            instructions: vec![
                instruction(ty, Operation::State),
                instruction(ty, Operation::Argument(0)),
            ],
            state: 1,
            reply: 0,
        };
        build(&program, &directory.artifact()).unwrap();
        assert_eq!(Manifest::read(&directory.artifact()).unwrap().state, ty);
    }
}

#[test]
fn builds_initializer_only_programs() {
    let directory = Directory::new();
    let mut program = program();
    program.handlers.clear();
    build(&program, &directory.artifact()).unwrap();
    assert!(
        Manifest::read(&directory.artifact())
            .unwrap()
            .handlers
            .is_empty()
    );
}

#[test]
fn rejects_invalid_frame_limits_without_creating_output() {
    let directory = Directory::new();
    for limit in [0, MAX_SCALAR_FRAME_BYTES + 1, u32::MAX] {
        let error = build_with_frame_limit(&program(), &directory.artifact(), limit).unwrap_err();
        assert!(
            error.contains("frame limit must be within 1..=65536"),
            "{error}"
        );
        assert!(!directory.artifact().exists());
    }
}

#[test]
fn rejects_actual_initializer_frame_before_creating_output() {
    let directory = Directory::new();
    let error = build_with_frame_limit(&program(), &directory.artifact(), 1).unwrap_err();
    assert!(
        error.contains("scalar stack frame for wings_scalar_init"),
        "{error}"
    );
    assert!(error.contains("limit is 1"), "{error}");
    assert!(!directory.artifact().exists());
}

#[test]
fn rejects_handler_register_pressure_before_creating_output() {
    let directory = Directory::new();
    let mut program = program();
    let mut instructions = vec![
        instruction(ValueType::Int, Operation::State),
        instruction(ValueType::Int, Operation::Argument(0)),
    ];
    let mut live = Vec::new();
    for value in 0..128 {
        let constant = instructions.len();
        instructions.push(instruction(ValueType::Int, Operation::Constant(value)));
        live.push(instructions.len());
        instructions.push(instruction(
            ValueType::Int,
            Operation::Binary(Operator::Add, 1, constant),
        ));
    }
    let mut total = live[0];
    for register in live.into_iter().skip(1) {
        let next = instructions.len();
        instructions.push(instruction(
            ValueType::Int,
            Operation::Binary(Operator::Add, total, register),
        ));
        total = next;
    }
    program.handlers[0].function = Function {
        instructions,
        state: 0,
        reply: total,
    };
    let error = build_with_frame_limit(&program, &directory.artifact(), 512).unwrap_err();
    assert!(
        error.contains("scalar stack frame for wings_scalar_handler_0"),
        "{error}"
    );
    assert!(error.contains("limit is 512"), "{error}");
    assert!(!directory.artifact().exists());
}

#[test]
fn rejects_invalid_ir_without_creating_output() {
    let directory = Directory::new();
    let mut program = program();
    program.handlers[0].function.instructions[2].operation = Operation::Negate(usize::MAX);
    let error = build(&program, &directory.artifact()).unwrap_err();
    assert!(error.contains("invalid scalar IR"), "{error}");
    assert!(!directory.artifact().exists());
}

#[test]
fn rejects_invalid_manifest_names_before_creating_output() {
    let directory = Directory::new();
    let mut program = program();
    program.name.clear();
    program.validate().unwrap();
    assert!(
        build(&program, &directory.artifact())
            .unwrap_err()
            .contains("process name")
    );
    assert!(!directory.artifact().exists());
}

#[test]
fn rejects_duplicate_handler_and_parameter_metadata() {
    let directory = Directory::new();
    let mut program = program();
    program.handlers.push(program.handlers[0].clone());
    assert!(
        build(&program, &directory.artifact())
            .unwrap_err()
            .contains("duplicate handler")
    );
    program.handlers.pop();
    program.handlers[0]
        .parameters
        .push(("delta".into(), ValueType::Bool));
    assert!(
        build(&program, &directory.artifact())
            .unwrap_err()
            .contains("duplicate parameter")
    );
    assert!(!directory.artifact().exists());
}

#[test]
fn rejects_oversized_manifest_before_codegen_and_directory_creation() {
    let directory = Directory::new();
    let mut program = program();
    let template = program.handlers[0].clone();
    program.handlers = (0..128)
        .map(|index| {
            let mut handler = template.clone();
            handler.name = format!("Handler{index}");
            handler.parameters = (0..64)
                .map(|parameter| (format!("p{parameter}"), ValueType::Int))
                .collect();
            handler
        })
        .collect();
    program.validate().unwrap();
    assert!(
        build(&program, &directory.artifact())
            .unwrap_err()
            .contains("exceeds 64 KiB")
    );
    assert!(!directory.artifact().exists());
}

#[cfg(unix)]
#[test]
fn refuses_symlink_output_without_modifying_its_target() {
    let directory = Directory::new();
    let target = directory.0.join("existing");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("manifest.json"), b"original").unwrap();
    std::os::unix::fs::symlink(&target, directory.artifact()).unwrap();
    assert!(
        build(&program(), &directory.artifact())
            .unwrap_err()
            .contains("create new artifact directory")
    );
    assert_eq!(fs::read(target.join("manifest.json")).unwrap(), b"original");
    assert_eq!(fs::read_dir(target).unwrap().count(), 1);
}

#[test]
fn existing_output_directory_is_never_overwritten() {
    let directory = Directory::new();
    fs::create_dir(directory.artifact()).unwrap();
    let sentinel = directory.artifact().join("manifest.json");
    fs::write(&sentinel, b"original").unwrap();
    assert!(
        build(&program(), &directory.artifact())
            .unwrap_err()
            .contains("create new artifact directory")
    );
    assert_eq!(fs::read(sentinel).unwrap(), b"original");
    assert_eq!(fs::read_dir(directory.artifact()).unwrap().count(), 1);
}

#[test]
fn existing_file_is_never_overwritten_and_missing_parents_are_not_created() {
    let directory = Directory::new();
    fs::write(directory.artifact(), b"original").unwrap();
    assert!(
        build(&program(), &directory.artifact())
            .unwrap_err()
            .contains("create new artifact directory")
    );
    assert_eq!(fs::read(directory.artifact()).unwrap(), b"original");
    let nested = directory.0.join("missing").join("artifact");
    assert!(
        build(&program(), &nested)
            .unwrap_err()
            .contains("create new artifact directory")
    );
    assert!(!directory.0.join("missing").exists());
}

#[test]
fn supports_paths_with_spaces_and_non_ascii_characters() {
    let directory = Directory::new();
    let artifact = directory.0.join("native artifact 产物");
    build(&program(), &artifact).unwrap();
    Manifest::read(&artifact).unwrap();
    assert!(artifact.join(library_name()).is_file());
}
