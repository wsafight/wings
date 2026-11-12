use wings_abi::ValueType;
use wings_ir::{
    Function, Handler, Instruction, MAX_HANDLERS, MAX_OPERATIONS, MAX_PARAMETERS, Operation,
    Operator, Program,
};

fn instruction(ty: ValueType, operation: Operation) -> Instruction {
    Instruction { ty, operation }
}

fn valid_program() -> Program {
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

fn rejects(program: &Program, message: &str) {
    let error = program.validate().unwrap_err();
    assert!(error.contains(message), "unexpected error: {error}");
}

#[test]
fn accepts_scalar_state_argument_and_arithmetic() {
    let mut program = valid_program();
    for operator in [
        Operator::Add,
        Operator::Subtract,
        Operator::Multiply,
        Operator::Divide,
    ] {
        program.handlers[0].function.instructions[2].operation = Operation::Binary(operator, 0, 1);
        program.validate().unwrap();
    }
}

#[test]
fn accepts_backwards_negation_and_nonfinal_output_registers() {
    let mut program = valid_program();
    let function = &mut program.handlers[0].function;
    function
        .instructions
        .push(instruction(ValueType::Int, Operation::Negate(2)));
    // Output registers need not be the last instruction or even the same register.
    function.state = 0;
    function.reply = 3;
    program.validate().unwrap();
}

#[test]
fn accepts_each_scalar_type_for_state_and_reply() {
    for ty in [ValueType::Int, ValueType::Bool, ValueType::Unit] {
        let mut program = valid_program();
        program.state = ty;
        program.initializer.instructions[0].ty = ty;
        let handler = &mut program.handlers[0];
        handler.parameters[0].1 = ty;
        handler.reply = Some(ty);
        handler.function = Function {
            instructions: vec![
                instruction(ty, Operation::State),
                instruction(ty, Operation::Argument(0)),
            ],
            state: 0,
            reply: 1,
        };
        program.validate().unwrap();
    }
}

#[test]
fn accepts_no_handlers_and_the_maximum_handler_count() {
    let mut program = valid_program();
    let handler = program.handlers[0].clone();
    program.handlers.clear();
    program.validate().unwrap();
    program.handlers = vec![handler; MAX_HANDLERS];
    program.validate().unwrap();
}

#[test]
fn rejects_one_more_than_the_handler_limit() {
    let mut program = valid_program();
    program.handlers = vec![program.handlers[0].clone(); MAX_HANDLERS + 1];
    rejects(&program, "too many handlers");
}

#[test]
fn accepts_exact_parameter_limit_and_rejects_one_more() {
    let mut program = valid_program();
    program.handlers[0].parameters = (0..MAX_PARAMETERS)
        .map(|index| (format!("p{index}"), ValueType::Int))
        .collect();
    program.handlers[0].function.instructions[1].operation =
        Operation::Argument(MAX_PARAMETERS - 1);
    program.validate().unwrap();
    program.handlers[0]
        .parameters
        .push(("extra".into(), ValueType::Int));
    rejects(&program, "too many parameters");
}

#[test]
fn accepts_exact_operation_limit_and_rejects_one_more_in_each_entry() {
    for initializer in [true, false] {
        let mut program = valid_program();
        let function = if initializer {
            &mut program.initializer
        } else {
            &mut program.handlers[0].function
        };
        function.instructions.resize(
            MAX_OPERATIONS,
            instruction(ValueType::Int, Operation::Constant(0)),
        );
        program.validate().unwrap();
        let function = if initializer {
            &mut program.initializer
        } else {
            &mut program.handlers[0].function
        };
        function
            .instructions
            .push(instruction(ValueType::Int, Operation::Constant(0)));
        rejects(&program, "too many operations");
    }
}

#[test]
fn rejects_state_reads_in_the_initializer() {
    let mut program = valid_program();
    program.initializer.instructions[0].operation = Operation::State;
    rejects(&program, "operand, type or forward reference");
}

#[test]
fn rejects_arguments_in_the_initializer() {
    let mut program = valid_program();
    program.initializer.instructions[0].operation = Operation::Argument(0);
    rejects(&program, "operand, type or forward reference");
}

#[test]
fn rejects_state_reads_with_the_wrong_type() {
    let mut program = valid_program();
    program.handlers[0].function.instructions[0].ty = ValueType::Bool;
    rejects(&program, "operand, type or forward reference");
}

#[test]
fn rejects_out_of_bounds_arguments_and_argument_type_mismatches() {
    for (ty, index) in [
        (ValueType::Int, 1),
        (ValueType::Int, usize::MAX),
        (ValueType::Bool, 0),
    ] {
        let mut program = valid_program();
        program.handlers[0].function.instructions[1] = instruction(ty, Operation::Argument(index));
        rejects(&program, "operand, type or forward reference");
    }
}

#[test]
fn rejects_invalid_boolean_and_unit_constant_encodings() {
    for (ty, values) in [
        (ValueType::Bool, vec![i64::MIN, -1, 2, i64::MAX]),
        (ValueType::Unit, vec![-1, 1]),
    ] {
        for value in values {
            let mut program = valid_program();
            program
                .initializer
                .instructions
                .push(instruction(ty, Operation::Constant(value)));
            rejects(&program, "operand, type or forward reference");
        }
    }
}

#[test]
fn accepts_integer_extremes_and_both_boolean_encodings() {
    let mut program = valid_program();
    for (ty, value) in [
        (ValueType::Int, i64::MIN),
        (ValueType::Int, i64::MAX),
        (ValueType::Bool, 0),
        (ValueType::Bool, 1),
    ] {
        program
            .initializer
            .instructions
            .push(instruction(ty, Operation::Constant(value)));
    }
    program.validate().unwrap();
}

#[test]
fn rejects_self_forward_and_missing_register_references() {
    for index in [2, 3, usize::MAX] {
        for operation in [
            Operation::Negate(index),
            Operation::Binary(Operator::Add, index, 0),
            Operation::Binary(Operator::Add, 0, index),
        ] {
            let mut program = valid_program();
            program.handlers[0].function.instructions[2].operation = operation;
            rejects(&program, "operand, type or forward reference");
        }
    }
}

#[test]
fn rejects_noninteger_arithmetic_operands_and_results() {
    for operation in [
        Operation::Negate(1),
        Operation::Binary(Operator::Multiply, 0, 1),
        Operation::Binary(Operator::Divide, 1, 0),
    ] {
        let mut program = valid_program();
        program.handlers[0].parameters[0].1 = ValueType::Bool;
        program.handlers[0].function.instructions[1].ty = ValueType::Bool;
        program.handlers[0].function.instructions[2].operation = operation;
        rejects(&program, "operand, type or forward reference");
    }
    let mut program = valid_program();
    program.handlers[0].function.instructions[2].ty = ValueType::Bool;
    rejects(&program, "operand, type or forward reference");
}

#[test]
fn rejects_empty_functions_and_missing_output_registers() {
    for initializer in [true, false] {
        for output in [0, 1] {
            let mut program = valid_program();
            let function = if initializer {
                &mut program.initializer
            } else {
                &mut program.handlers[0].function
            };
            if output == 0 {
                function.state = usize::MAX;
            } else {
                function.reply = usize::MAX;
            }
            rejects(&program, "state or reply register");
        }
        let mut program = valid_program();
        if initializer {
            program.initializer.instructions.clear();
        } else {
            program.handlers[0].function.instructions.clear();
        }
        rejects(&program, "state or reply register");
    }
}

#[test]
fn rejects_output_registers_of_the_wrong_type() {
    let mut program = valid_program();
    program.initializer.state = 1;
    rejects(&program, "state or reply register");
    let mut program = valid_program();
    program.initializer.reply = 0;
    rejects(&program, "state or reply register");
    let mut program = valid_program();
    program.handlers[0].reply = Some(ValueType::Bool);
    rejects(&program, "state or reply register");
}

#[test]
fn one_way_and_explicit_unit_handlers_require_unit_reply_registers() {
    for reply in [None, Some(ValueType::Unit)] {
        let mut program = valid_program();
        program.handlers[0].reply = reply;
        rejects(&program, "state or reply register");
        program.handlers[0]
            .function
            .instructions
            .push(instruction(ValueType::Unit, Operation::Constant(0)));
        program.handlers[0].function.reply = 3;
        program.validate().unwrap();
    }
}

#[test]
fn arithmetic_faults_are_runtime_errors_not_ir_validation_errors() {
    let mut program = valid_program();
    // These are well typed even though native execution must return a managed
    // arithmetic error. No faulty native code is executed by this test.
    program.handlers[0].function.instructions = vec![
        instruction(ValueType::Int, Operation::Constant(i64::MIN)),
        instruction(ValueType::Int, Operation::Constant(0)),
        instruction(ValueType::Int, Operation::Negate(0)),
        instruction(ValueType::Int, Operation::Binary(Operator::Divide, 0, 1)),
    ];
    program.handlers[0].function.state = 0;
    program.handlers[0].function.reply = 3;
    program.validate().unwrap();
}
