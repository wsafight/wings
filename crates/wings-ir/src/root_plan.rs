use std::collections::BTreeSet;

use super::{Function, Operation, Register, RootPlan, operands};

pub(super) fn build(function: &Function) -> RootPlan {
    let last_uses = legacy_last_uses(function);
    let mut graph = Graph::new(function);
    graph.solve();
    graph.finish(last_uses)
}

/// Retain the historical linear last-use diagnostic without using it to decide
/// which values are rooted at a checkpoint. Loops have no single linear last
/// use, so their diagnostic endpoint remains the matching end-loop.
fn legacy_last_uses(function: &Function) -> Vec<usize> {
    let mut last_uses: Vec<usize> = (0..function.instructions.len()).collect();
    for (index, instruction) in function.instructions.iter().enumerate() {
        for operand in operands(&instruction.operation) {
            if let Some(last) = last_uses.get_mut(operand) {
                *last = index;
            }
        }
    }
    for output in [function.state, function.reply] {
        if let Some(last) = last_uses.get_mut(output) {
            *last = function.instructions.len();
        }
    }
    let mut loops = Vec::new();
    for (index, instruction) in function.instructions.iter().enumerate() {
        match instruction.operation {
            Operation::Loop(_) => loops.push(index),
            Operation::While(_) => {
                if let Some(start) = loops.last() {
                    last_uses[*start] = last_uses[*start].max(index);
                }
            }
            Operation::EndLoop(_) => {
                if let Some(start) = loops.pop() {
                    for last in &mut last_uses[..start] {
                        if *last > start && *last < index {
                            *last = index;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    last_uses
}

#[derive(Clone, Copy)]
struct Edge {
    target: usize,
    phi_input: Option<Register>,
}

struct Node {
    successors: Vec<Edge>,
    uses: BTreeSet<Register>,
    definition: Option<Register>,
    defined_before_poll: bool,
    live_out: BTreeSet<Register>,
    required_in: BTreeSet<Register>,
    at: BTreeSet<Register>,
}

impl Node {
    fn new() -> Self {
        Self {
            successors: Vec::new(),
            uses: BTreeSet::new(),
            definition: None,
            defined_before_poll: false,
            live_out: BTreeSet::new(),
            required_in: BTreeSet::new(),
            at: BTreeSet::new(),
        }
    }
}

enum Region {
    Branch {
        if_node: usize,
        else_node: Option<(usize, Register)>,
    },
    Loop {
        header: usize,
        while_node: Option<usize>,
    },
}

struct Graph<'a> {
    function: &'a Function,
    nodes: Vec<Node>,
}

impl<'a> Graph<'a> {
    fn new(function: &'a Function) -> Self {
        let mut graph = Self {
            function,
            nodes: (0..=function.instructions.len())
                .map(|_| Node::new())
                .collect(),
        };
        graph.construct();
        graph
    }

    fn point(&mut self, index: usize) -> usize {
        let instruction = &self.function.instructions[index];
        let mut result = Node::new();
        if instruction.ty.is_reference() {
            result.definition = Some(index);
            result.defined_before_poll = matches!(
                instruction.operation,
                Operation::Loop(_) | Operation::EndIf(_)
            );
        }
        if !matches!(
            instruction.operation,
            Operation::Loop(_) | Operation::EndIf(_)
        ) {
            result.uses = self.references(operands(&instruction.operation));
        }
        self.nodes[index] = result;
        index
    }

    fn virtual_node(&mut self, definition: Option<Register>) -> usize {
        let node = self.nodes.len();
        let mut result = Node::new();
        result.definition = definition;
        result.defined_before_poll = true;
        self.nodes.push(result);
        node
    }

    fn connect(&mut self, source: usize, target: usize, phi_input: Option<Register>) {
        let phi_input = phi_input.filter(|register| self.is_reference(*register));
        let duplicate = self.nodes[source]
            .successors
            .iter()
            .any(|edge| edge.target == target && edge.phi_input == phi_input);
        if !duplicate {
            self.nodes[source]
                .successors
                .push(Edge { target, phi_input });
        }
    }

    fn fallthrough(&mut self, source: Option<usize>, target: usize, phi_input: Option<Register>) {
        if let Some(source) = source {
            self.connect(source, target, phi_input);
        }
    }

    fn references(&self, registers: impl Iterator<Item = Register>) -> BTreeSet<Register> {
        registers
            .filter(|register| self.is_reference(*register))
            .collect()
    }

    fn is_reference(&self, register: Register) -> bool {
        self.function
            .instructions
            .get(register)
            .is_some_and(|instruction| instruction.ty.is_reference())
    }

    fn construct(&mut self) {
        let mut current = None;
        let mut regions = Vec::new();
        for index in 0..self.function.instructions.len() {
            let operation = &self.function.instructions[index].operation;
            match *operation {
                Operation::Loop(initial) => {
                    let header = self.point(index);
                    self.fallthrough(current, header, Some(initial));
                    current = Some(header);
                    regions.push(Region::Loop {
                        header,
                        while_node: None,
                    });
                }
                Operation::While(_) => {
                    let while_node = self.point(index);
                    self.fallthrough(current, while_node, None);
                    current = Some(while_node);
                    if let Some(Region::Loop {
                        while_node: tested, ..
                    }) = regions.last_mut()
                    {
                        *tested = Some(while_node);
                    }
                }
                Operation::EndLoop(value) => {
                    let end = self.point(index);
                    self.fallthrough(current, end, None);
                    let Some(Region::Loop { header, while_node }) = regions.pop() else {
                        current = Some(end);
                        continue;
                    };
                    self.connect(end, header, Some(value));
                    let definition = self.function.instructions[index]
                        .ty
                        .is_reference()
                        .then_some(index);
                    let after = self.virtual_node(definition);
                    if let Some(while_node) = while_node {
                        self.connect(while_node, after, Some(header));
                    }
                    current = Some(after);
                }
                Operation::If(_) => {
                    let if_node = self.point(index);
                    self.fallthrough(current, if_node, None);
                    current = Some(if_node);
                    regions.push(Region::Branch {
                        if_node,
                        else_node: None,
                    });
                }
                Operation::Else(value) => {
                    let else_node = self.point(index);
                    self.fallthrough(current, else_node, None);
                    let after_else = self.virtual_node(None);
                    if let Some(Region::Branch {
                        if_node,
                        else_node: slot,
                    }) = regions.last_mut()
                    {
                        self.connect(*if_node, after_else, None);
                        *slot = Some((else_node, value));
                    }
                    current = Some(after_else);
                }
                Operation::EndIf(value) => {
                    let merge = self.point(index);
                    self.fallthrough(current, merge, Some(value));
                    if let Some(Region::Branch {
                        else_node: Some((else_node, else_value)),
                        ..
                    }) = regions.pop()
                    {
                        self.connect(else_node, merge, Some(else_value));
                    }
                    current = Some(merge);
                }
                _ => {
                    let point = self.point(index);
                    self.fallthrough(current, point, None);
                    current = Some(point);
                }
            }
        }

        let final_node = self.function.instructions.len();
        let mut final_point = Node::new();
        final_point.uses = self.references(
            [self.function.state, self.function.reply]
                .into_iter()
                .filter(|register| *register < self.function.instructions.len()),
        );
        self.nodes[final_node] = final_point;
        self.fallthrough(current, final_node, None);
    }

    fn solve(&mut self) {
        loop {
            let mut changed = false;
            for index in (0..self.nodes.len()).rev() {
                let mut live_out = BTreeSet::new();
                for edge in &self.nodes[index].successors {
                    live_out.extend(self.nodes[edge.target].required_in.iter().copied());
                    live_out.extend(edge.phi_input);
                }

                let mut required_in = self.nodes[index].uses.clone();
                required_in.extend(live_out.iter().copied());
                if let Some(definition) = self.nodes[index].definition {
                    required_in.remove(&definition);
                }

                let mut at = required_in.clone();
                if self.nodes[index].defined_before_poll {
                    if let Some(definition) = self.nodes[index].definition {
                        if live_out.contains(&definition) {
                            at.insert(definition);
                        }
                    }
                }

                if self.nodes[index].live_out != live_out
                    || self.nodes[index].required_in != required_in
                    || self.nodes[index].at != at
                {
                    self.nodes[index].live_out = live_out;
                    self.nodes[index].required_in = required_in;
                    self.nodes[index].at = at;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn finish(self, last_uses: Vec<usize>) -> RootPlan {
        let instruction_count = self.function.instructions.len();
        let mut at = Vec::with_capacity(instruction_count + 1);
        let mut capacity = 0;
        for node in &self.nodes[..=instruction_count] {
            capacity = capacity.max(node.at.len());
            at.push(node.at.iter().copied().collect());
        }
        RootPlan {
            last_uses,
            capacity,
            at,
        }
    }
}
