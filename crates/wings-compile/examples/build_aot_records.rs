use std::path::Path;

use wings_abi::{RecordSchema, ValueType};
use wings_ir::{Function, Handler, Instruction, Operation, Operator, Program};

fn push(instructions: &mut Vec<Instruction>, ty: ValueType, operation: Operation) -> usize {
    let register = instructions.len();
    instructions.push(Instruction { ty, operation });
    register
}

fn new_box(instructions: &mut Vec<Instruction>, value: usize) -> usize {
    push(
        instructions,
        ValueType::RecordRef(0),
        Operation::RecordNew {
            schema: 0,
            arguments: vec![value],
        },
    )
}

fn read_box(instructions: &mut Vec<Instruction>, value: usize) -> usize {
    push(
        instructions,
        ValueType::Int,
        Operation::RecordField { value, field: 0 },
    )
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert_eq!(arguments.len(), 1);
    let mut initializer = Vec::new();
    push(&mut initializer, ValueType::Int, Operation::Constant(0));
    push(&mut initializer, ValueType::Unit, Operation::Constant(0));
    let mut program = Program {
        records: vec![RecordSchema {
            name: "Boxed".into(),
            fields: vec![("value".into(), ValueType::Int)],
        }],
        external_actors: Vec::new(),
        name: "Records".into(),
        state: ValueType::Int,
        functions: Vec::new(),
        initializer: Function {
            instructions: initializer,
            state: 0,
            reply: 1,
        },
        handlers: Vec::new(),
    };
    for name in ["Live", "Full", "Fail", "Reuse", "Many", "Integer", "Get"] {
        let mut instructions = Vec::new();
        let seed = push(&mut instructions, ValueType::Int, Operation::Constant(11));
        let mut reply = seed;
        if name == "Get" {
            reply = push(&mut instructions, ValueType::Int, Operation::State);
        } else if name == "Integer" {
            let reference = new_box(&mut instructions, seed);
            read_box(&mut instructions, reference);
            reply = push(
                &mut instructions,
                ValueType::Int,
                Operation::Constant(1_i64 << 32),
            );
            push(&mut instructions, ValueType::Unit, Operation::Collect);
        } else if matches!(name, "Live" | "Full" | "Many") {
            let first = new_box(&mut instructions, seed);
            let mut references = vec![first];
            if name == "Live" {
                for _ in 0..6 {
                    new_box(&mut instructions, seed);
                }
            } else {
                for _ in 1..if name == "Full" { 3 } else { 64 } {
                    references.push(new_box(&mut instructions, seed));
                }
            }
            push(&mut instructions, ValueType::Unit, Operation::Collect);
            for reference in references {
                let value = read_box(&mut instructions, reference);
                reply = push(
                    &mut instructions,
                    ValueType::Int,
                    Operation::Binary(Operator::Add, reply, value),
                );
            }
        } else if name == "Fail" {
            let reference = new_box(&mut instructions, seed);
            let zero = push(&mut instructions, ValueType::Int, Operation::Constant(0));
            push(
                &mut instructions,
                ValueType::Int,
                Operation::Binary(Operator::Divide, seed, zero),
            );
            reply = read_box(&mut instructions, reference);
        } else {
            for _ in 0..128 {
                let reference = new_box(&mut instructions, seed);
                reply = read_box(&mut instructions, reference);
            }
        }
        push(&mut instructions, ValueType::Unit, Operation::Collect);
        program.handlers.push(Handler {
            name: name.into(),
            parameters: Vec::new(),
            reply: Some(ValueType::Int),
            completion: None,
            function: Function {
                instructions,
                state: reply,
                reply,
            },
        });
    }
    let directory = Path::new(&arguments[0]);
    wings_codegen::build(&program, &directory.join("records")).expect("record IR compilation");
    let mut initializer = Vec::new();
    let seed = push(&mut initializer, ValueType::Int, Operation::Constant(7));
    let reference = new_box(&mut initializer, seed);
    push(&mut initializer, ValueType::Unit, Operation::Collect);
    let state = read_box(&mut initializer, reference);
    let reply = push(&mut initializer, ValueType::Unit, Operation::Collect);
    program.initializer = Function {
        instructions: initializer,
        state,
        reply,
    };
    wings_codegen::build(&program, &directory.join("initializer"))
        .expect("record initializer compilation");
    println!("Built internal AOT record/root fixtures (no language syntax extension)");
}
