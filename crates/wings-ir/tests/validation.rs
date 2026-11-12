use wings_abi::ValueType;
use wings_ir::{
    Comparison, Function, Handler, Instruction, MAX_HANDLERS, MAX_OPERATIONS, MAX_PARAMETERS,
    Operation, Operator, Program,
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
fn record_merges_publish_only_outer_and_selected_values_before_the_merge_poll() {
    let mut program = valid_program();
    let function = &mut program.handlers[0].function;
    function.instructions = vec![
        instruction(ValueType::Int, Operation::State),
        instruction(ValueType::Ref, Operation::RecordInt(0)),
        instruction(ValueType::Bool, Operation::Constant(1)),
        instruction(ValueType::Unit, Operation::If(2)),
        instruction(ValueType::Ref, Operation::RecordInt(0)),
        instruction(ValueType::Unit, Operation::Else(4)),
        instruction(ValueType::Ref, Operation::RecordInt(0)),
        instruction(ValueType::Ref, Operation::EndIf(6)),
        instruction(ValueType::Unit, Operation::Collect),
        instruction(ValueType::Int, Operation::ReadInt(7)),
        instruction(ValueType::Int, Operation::ReadInt(1)),
        instruction(ValueType::Int, Operation::Binary(Operator::Add, 9, 10)),
    ];
    function.reply = 11;
    function.state = 0;
    program.validate().unwrap();
    let plan = program.handlers[0].function.root_plan();
    assert_eq!(plan.capacity, 2);
    assert_eq!(plan.at[5], vec![1, 4]);
    assert_eq!(plan.at[6], vec![1]);
    assert_eq!(plan.at[7], vec![1, 7]);
    assert_eq!(plan.at[8], vec![1, 7]);
    assert_eq!(plan.at[10], vec![1]);
    for branch_local in [4, 6] {
        let mut invalid = program.clone();
        invalid.handlers[0].function.instructions[9].operation = Operation::ReadInt(branch_local);
        rejects(&invalid, "branch scope");
    }
    let mut invalid = program.clone();
    invalid.handlers[0].function.instructions[7].ty = ValueType::Int;
    rejects(&invalid, "operand, type");
    let mut dead = program;
    dead.handlers[0].function.instructions[9].operation = Operation::ReadInt(1);
    dead.validate().unwrap();
    assert_eq!(dead.handlers[0].function.root_plan().at[7], vec![1]);
}

#[test]
fn nested_record_merges_restore_path_roots_within_loops() {
    let mut program = valid_program();
    let function = &mut program.handlers[0].function;
    function.instructions = vec![
        instruction(ValueType::Int, Operation::State),
        instruction(ValueType::Ref, Operation::RecordInt(0)),
        instruction(ValueType::Int, Operation::Loop(0)),
        instruction(ValueType::Bool, Operation::Constant(1)),
        instruction(ValueType::Unit, Operation::While(3)),
        instruction(ValueType::Unit, Operation::If(3)),
        instruction(ValueType::Ref, Operation::RecordInt(2)),
        instruction(ValueType::Unit, Operation::If(3)),
        instruction(ValueType::Ref, Operation::RecordInt(2)),
        instruction(ValueType::Unit, Operation::Else(8)),
        instruction(ValueType::Ref, Operation::EndIf(6)),
        instruction(ValueType::Unit, Operation::Else(10)),
        instruction(ValueType::Ref, Operation::EndIf(1)),
        instruction(ValueType::Int, Operation::ReadInt(12)),
        instruction(ValueType::Int, Operation::EndLoop(13)),
    ];
    function.state = 14;
    function.reply = 14;
    program.validate().unwrap();
    let plan = program.handlers[0].function.root_plan();
    assert_eq!(plan.at[10], vec![1, 6, 10]);
    assert_eq!(plan.at[12], vec![1, 12]);
    assert_eq!(plan.at[14], vec![1]);
    assert!(plan.at[15].is_empty());
    assert_eq!(plan.capacity, 3);
}

#[test]
fn record_merge_itself_counts_toward_root_admission_before_its_poll() {
    for outer_count in [63, 64] {
        let mut program = valid_program();
        let function = &mut program.handlers[0].function;
        function.instructions.truncate(1);
        for _ in 0..outer_count {
            function
                .instructions
                .push(instruction(ValueType::Ref, Operation::RecordInt(0)));
        }
        let condition = function.instructions.len();
        function
            .instructions
            .push(instruction(ValueType::Bool, Operation::Constant(1)));
        function
            .instructions
            .push(instruction(ValueType::Unit, Operation::If(condition)));
        let then_value = function.instructions.len();
        function
            .instructions
            .push(instruction(ValueType::Ref, Operation::RecordInt(0)));
        function
            .instructions
            .push(instruction(ValueType::Unit, Operation::Else(then_value)));
        let else_value = function.instructions.len();
        function
            .instructions
            .push(instruction(ValueType::Ref, Operation::RecordInt(0)));
        let merged = function.instructions.len();
        function
            .instructions
            .push(instruction(ValueType::Ref, Operation::EndIf(else_value)));
        function
            .instructions
            .push(instruction(ValueType::Int, Operation::ReadInt(merged)));
        for reference in 1..=outer_count {
            function
                .instructions
                .push(instruction(ValueType::Int, Operation::ReadInt(reference)));
        }
        function.state = 0;
        function.reply = function.instructions.len() - 1;
        assert_eq!(function.root_plan().capacity, outer_count + 1);
        if outer_count == 63 {
            program.validate().unwrap();
        } else {
            rejects(&program, "64 simultaneously live");
        }
    }
}

#[test]
fn loop_structure_types_scope_and_extended_reference_lifetimes_are_validated() {
    fn loop_program() -> Program {
        let mut program = valid_program();
        let function = &mut program.handlers[0].function;
        function.instructions = vec![
            instruction(ValueType::Int, Operation::State),
            instruction(ValueType::Ref, Operation::RecordInt(0)),
            instruction(ValueType::Int, Operation::Loop(0)),
            instruction(ValueType::Bool, Operation::Constant(1)),
            instruction(ValueType::Unit, Operation::While(3)),
            instruction(ValueType::Int, Operation::ReadInt(1)),
            instruction(ValueType::Int, Operation::EndLoop(5)),
        ];
        function.state = 6;
        function.reply = 6;
        program
    }
    let program = loop_program();
    program.validate().unwrap();
    let plan = program.handlers[0].function.root_plan();
    assert_eq!(plan.last_uses[1], 6);
    assert_eq!(plan.capacity, 1);
    for (index, replacement, message) in [
        (
            2,
            instruction(ValueType::Ref, Operation::Loop(1)),
            "operand, type",
        ),
        (
            2,
            instruction(ValueType::Unit, Operation::While(3)),
            "operand, type",
        ),
        (
            3,
            instruction(ValueType::Int, Operation::Constant(1)),
            "operand, type",
        ),
        (
            4,
            instruction(ValueType::Unit, Operation::If(3)),
            "end-loop without",
        ),
        (
            4,
            instruction(ValueType::Unit, Operation::Collect),
            "end-loop without",
        ),
        (
            5,
            instruction(ValueType::Unit, Operation::While(3)),
            "duplicate while",
        ),
        (
            5,
            instruction(ValueType::Unit, Operation::Else(0)),
            "else without",
        ),
        (
            6,
            instruction(ValueType::Bool, Operation::EndLoop(3)),
            "operand, type",
        ),
        (
            6,
            instruction(ValueType::Int, Operation::Constant(0)),
            "unterminated",
        ),
    ] {
        let mut program = loop_program();
        program.handlers[0].function.instructions[index] = replacement;
        rejects(&program, message);
    }
    for register in [2, 3, 5] {
        let mut program = loop_program();
        program.handlers[0]
            .function
            .instructions
            .push(instruction(ValueType::Int, Operation::Negate(register)));
        rejects(&program, "branch scope");
    }
    let mut program = loop_program();
    program.initializer = program.handlers[0].function.clone();
    rejects(&program, "operand, type");
    let mut program = loop_program();
    let function = &mut program.handlers[0].function;
    function
        .instructions
        .insert(5, instruction(ValueType::Int, Operation::Loop(2)));
    function
        .instructions
        .insert(6, instruction(ValueType::Unit, Operation::While(3)));
    function
        .instructions
        .insert(8, instruction(ValueType::Int, Operation::EndLoop(7)));
    function.instructions[9].operation = Operation::EndLoop(8);
    function.state = 9;
    function.reply = 9;
    program.validate().unwrap();
    assert_eq!(program.handlers[0].function.root_plan().last_uses[1], 9);

    for depth in [64, 65] {
        let mut program = valid_program();
        let function = &mut program.handlers[0].function;
        function.instructions = vec![
            instruction(ValueType::Int, Operation::State),
            instruction(ValueType::Bool, Operation::Constant(0)),
        ];
        let mut value = 0;
        for _ in 0..depth {
            function
                .instructions
                .push(instruction(ValueType::Int, Operation::Loop(value)));
            value = function.instructions.len() - 1;
            function
                .instructions
                .push(instruction(ValueType::Unit, Operation::While(1)));
        }
        for _ in 0..depth {
            function
                .instructions
                .push(instruction(ValueType::Int, Operation::EndLoop(value)));
            value = function.instructions.len() - 1;
        }
        function.state = value;
        function.reply = value;
        if depth == 64 {
            program.validate().unwrap();
        } else {
            rejects(&program, "control nesting exceeds 64");
        }
    }
}

#[test]
fn reference_loop_headers_replace_initial_roots_and_keep_zero_exit_values() {
    let mut program = valid_program();
    program.state = ValueType::Ref;
    program
        .initializer
        .instructions
        .insert(1, instruction(ValueType::Ref, Operation::RecordInt(0)));
    program.initializer.state = 1;
    program.initializer.reply = 2;
    let function = &mut program.handlers[0].function;
    function.instructions = vec![
        instruction(ValueType::Ref, Operation::State),
        instruction(ValueType::Int, Operation::Constant(7)),
        instruction(ValueType::Ref, Operation::RecordInt(1)),
        instruction(ValueType::Ref, Operation::Loop(2)),
        instruction(ValueType::Bool, Operation::Constant(0)),
        instruction(ValueType::Unit, Operation::While(4)),
        instruction(ValueType::Ref, Operation::RecordInt(1)),
        instruction(ValueType::Ref, Operation::EndLoop(6)),
        instruction(ValueType::Int, Operation::ReadInt(7)),
    ];
    function.state = 7;
    function.reply = 8;
    program.validate().unwrap();
    let plan = program.handlers[0].function.root_plan();
    assert_eq!(plan.last_uses[2], 3);
    assert_eq!(plan.last_uses[3], 5);
    assert_eq!(plan.at[3], vec![3]);
    assert_eq!(plan.at[5], vec![3]);
    assert_eq!(plan.at[7], vec![6]);
    assert_eq!(plan.at[8], vec![7]);
    assert_eq!(plan.at[9], vec![7]);
    let mut wrong = program.clone();
    wrong.handlers[0].function.instructions[7].operation = Operation::EndLoop(1);
    rejects(&wrong, "operand, type");
    for local in [3, 6] {
        let mut wrong = program.clone();
        wrong.handlers[0].function.instructions[8].operation = Operation::ReadInt(local);
        rejects(&wrong, "branch scope");
    }
    let mut alias = program;
    alias.handlers[0]
        .function
        .instructions
        .push(instruction(ValueType::Int, Operation::ReadInt(2)));
    alias.handlers[0].function.reply = 9;
    alias.validate().unwrap();
    let plan = alias.handlers[0].function.root_plan();
    assert_eq!(plan.at[3], vec![2, 3]);
    assert_eq!(plan.at[8], vec![2, 7]);
}

#[test]
fn comparison_types_and_operands_are_validated_before_codegen() {
    for comparison in [
        Comparison::Equal,
        Comparison::NotEqual,
        Comparison::Less,
        Comparison::LessEqual,
        Comparison::Greater,
        Comparison::GreaterEqual,
    ] {
        let mut program = valid_program();
        program.handlers[0].reply = Some(ValueType::Bool);
        let function = &mut program.handlers[0].function;
        function.instructions[2] =
            instruction(ValueType::Bool, Operation::Compare(comparison, 0, 1));
        function.state = 0;
        program.validate().unwrap();
        program.handlers[0].function.instructions[2].ty = ValueType::Int;
        rejects(&program, "operand, type");
    }
    for (left, right) in [(usize::MAX, 1), (0, 2), (2, 0)] {
        let mut program = valid_program();
        program.handlers[0].function.instructions[2] = instruction(
            ValueType::Bool,
            Operation::Compare(Comparison::Equal, left, right),
        );
        rejects(&program, "operand, type");
    }
    for (ty, comparison, valid) in [
        (ValueType::Bool, Comparison::Equal, true),
        (ValueType::Unit, Comparison::NotEqual, true),
        (ValueType::Bool, Comparison::Less, false),
        (ValueType::Ref, Comparison::Equal, false),
    ] {
        let mut program = valid_program();
        let function = &mut program.handlers[0].function;
        function.instructions = vec![
            instruction(ValueType::Int, Operation::Constant(0)),
            instruction(
                ty,
                if ty == ValueType::Ref {
                    Operation::RecordInt(0)
                } else {
                    Operation::Constant(0)
                },
            ),
            instruction(ValueType::Bool, Operation::Compare(comparison, 1, 1)),
        ];
        function.state = 0;
        program.handlers[0].reply = Some(ValueType::Bool);
        if valid {
            program.validate().unwrap();
        } else {
            rejects(&program, "operand, type");
        }
    }
}

#[test]
fn conditional_structure_types_depth_and_branch_scopes_are_validated() {
    fn conditional() -> Program {
        let mut program = valid_program();
        program.initializer.instructions = vec![
            instruction(ValueType::Bool, Operation::Constant(1)),
            instruction(ValueType::Unit, Operation::If(0)),
            instruction(ValueType::Int, Operation::Constant(7)),
            instruction(ValueType::Unit, Operation::Else(2)),
            instruction(ValueType::Int, Operation::Constant(9)),
            instruction(ValueType::Int, Operation::EndIf(4)),
            instruction(ValueType::Unit, Operation::Constant(0)),
        ];
        program.initializer.state = 5;
        program.initializer.reply = 6;
        program
    }
    conditional().validate().unwrap();
    for (index, replacement, message) in [
        (
            0,
            instruction(ValueType::Int, Operation::Constant(1)),
            "operand, type",
        ),
        (
            1,
            instruction(ValueType::Unit, Operation::Else(0)),
            "else without",
        ),
        (
            3,
            instruction(ValueType::Unit, Operation::EndIf(2)),
            "end-if without",
        ),
        (
            4,
            instruction(ValueType::Int, Operation::Negate(2)),
            "branch scope",
        ),
        (
            4,
            instruction(ValueType::Int, Operation::Negate(3)),
            "branch scope",
        ),
        (
            4,
            instruction(ValueType::Unit, Operation::Else(0)),
            "duplicate else",
        ),
        (
            5,
            instruction(ValueType::Bool, Operation::EndIf(4)),
            "operand, type",
        ),
        (
            5,
            instruction(ValueType::Int, Operation::Constant(0)),
            "unterminated",
        ),
        (
            6,
            instruction(ValueType::Int, Operation::Negate(4)),
            "branch scope",
        ),
        (
            6,
            instruction(ValueType::Int, Operation::Negate(2)),
            "branch scope",
        ),
    ] {
        let mut program = conditional();
        program.initializer.instructions[index] = replacement;
        rejects(&program, message);
    }
    for state in [2, 4] {
        let mut program = conditional();
        program.initializer.state = state;
        rejects(&program, "state or reply register");
    }
    for depth in [64, 65] {
        let mut program = conditional();
        let function = &mut program.initializer;
        function.instructions.truncate(1);
        for _ in 0..depth {
            function
                .instructions
                .push(instruction(ValueType::Unit, Operation::If(0)));
        }
        function
            .instructions
            .push(instruction(ValueType::Int, Operation::Constant(7)));
        let mut value = function.instructions.len() - 1;
        for _ in 0..depth {
            function
                .instructions
                .push(instruction(ValueType::Unit, Operation::Else(value)));
            function
                .instructions
                .push(instruction(ValueType::Int, Operation::Constant(9)));
            function.instructions.push(instruction(
                ValueType::Int,
                Operation::EndIf(function.instructions.len() - 1),
            ));
            value = function.instructions.len() - 1;
        }
        function.state = value;
        function.reply = function.instructions.len();
        function
            .instructions
            .push(instruction(ValueType::Unit, Operation::Constant(0)));
        if depth == 64 {
            program.validate().unwrap();
        } else {
            rejects(&program, "nesting exceeds 64");
        }
    }
}

#[test]
fn temporary_records_are_well_typed_and_have_exact_last_uses() {
    let mut program = valid_program();
    let function = &mut program.handlers[0].function;
    function.instructions = vec![
        instruction(ValueType::Int, Operation::Constant(7)),
        instruction(ValueType::Ref, Operation::RecordInt(0)),
        instruction(ValueType::Unit, Operation::Collect),
        instruction(ValueType::Int, Operation::ReadInt(1)),
        instruction(ValueType::Ref, Operation::RecordInt(0)),
        instruction(ValueType::Unit, Operation::Collect),
    ];
    function.state = 0;
    function.reply = 3;
    let plan = function.root_plan();
    assert_eq!(plan.capacity, 1);
    assert_eq!(plan.last_uses[1], 3);
    assert_eq!(plan.last_uses[4], 4);
    program.validate().unwrap();
}

#[test]
fn rejects_record_forging_type_confusion_and_boundary_references() {
    for (ty, operation) in [
        (ValueType::Ref, Operation::Constant(1)),
        (ValueType::Ref, Operation::RecordInt(usize::MAX)),
        (ValueType::Int, Operation::RecordInt(0)),
        (ValueType::Int, Operation::ReadInt(0)),
        (ValueType::Int, Operation::Collect),
    ] {
        let mut program = valid_program();
        program.handlers[0]
            .function
            .instructions
            .push(instruction(ty, operation));
        rejects(&program, "operand, type or forward reference");
    }
    for boundary in 1..3 {
        let mut program = valid_program();
        match boundary {
            1 => program.handlers[0].parameters[0].1 = ValueType::Ref,
            _ => program.handlers[0].reply = Some(ValueType::Ref),
        }
        rejects(&program, "cannot cross scalar IR boundaries");
    }
}

#[test]
fn persistent_state_and_initializer_outputs_stay_live_through_final_checkpoint() {
    let mut program = valid_program();
    program.state = ValueType::Ref;
    program.initializer.instructions = vec![
        instruction(ValueType::Int, Operation::Constant(7)),
        instruction(ValueType::Ref, Operation::RecordInt(0)),
        instruction(ValueType::Unit, Operation::Collect),
    ];
    program.initializer.state = 1;
    program.initializer.reply = 2;
    program.handlers[0].function.instructions = vec![
        instruction(ValueType::Ref, Operation::State),
        instruction(ValueType::Int, Operation::Argument(0)),
        instruction(ValueType::Ref, Operation::RecordInt(1)),
        instruction(ValueType::Unit, Operation::Collect),
    ];
    program.handlers[0].function.state = 2;
    program.handlers[0].function.reply = 1;
    program.validate().unwrap();
    let plan = program.initializer.root_plan();
    assert_eq!(plan.last_uses[1], 3);
    assert_eq!(plan.capacity, 1);
    let plan = program.handlers[0].function.root_plan();
    assert_eq!(plan.last_uses[0], 0);
    assert_eq!(plan.last_uses[2], 4);
    assert_eq!(plan.capacity, 1);
}

#[test]
fn returned_reference_counts_toward_root_limit_even_without_later_operand_use() {
    for temporary in [63, 64] {
        let mut program = valid_program();
        let function = &mut program.initializer;
        function.instructions = vec![instruction(ValueType::Int, Operation::Constant(7))];
        for _ in 0..=temporary {
            function
                .instructions
                .push(instruction(ValueType::Ref, Operation::RecordInt(0)));
        }
        function.state = 1;
        for reference in 2..=temporary + 1 {
            function
                .instructions
                .push(instruction(ValueType::Int, Operation::ReadInt(reference)));
        }
        function.reply = function.instructions.len();
        function
            .instructions
            .push(instruction(ValueType::Unit, Operation::Constant(0)));
        assert_eq!(function.root_plan().capacity, temporary + 1);
        program.state = ValueType::Ref;
        program.handlers.clear();
        if temporary == 63 {
            program.validate().unwrap();
        } else {
            rejects(&program, "64 simultaneously live");
        }
    }
}

#[test]
fn reference_limit_counts_simultaneously_live_values_not_total_allocations() {
    for count in [64, 65] {
        let mut program = valid_program();
        let function = &mut program.handlers[0].function;
        function.instructions = vec![instruction(ValueType::Int, Operation::Constant(1))];
        for _ in 0..count {
            function
                .instructions
                .push(instruction(ValueType::Ref, Operation::RecordInt(0)));
        }
        for reference in 1..=count {
            function
                .instructions
                .push(instruction(ValueType::Int, Operation::ReadInt(reference)));
        }
        function.state = 0;
        function.reply = 0;
        assert_eq!(function.root_plan().capacity, count);
        if count == 64 {
            program.validate().unwrap();
        } else {
            rejects(&program, "64 simultaneously live");
        }
    }
    let mut program = valid_program();
    for _ in 0..128 {
        program.handlers[0]
            .function
            .instructions
            .push(instruction(ValueType::Ref, Operation::RecordInt(0)));
    }
    assert_eq!(program.handlers[0].function.root_plan().capacity, 0);
    program.validate().unwrap();
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
