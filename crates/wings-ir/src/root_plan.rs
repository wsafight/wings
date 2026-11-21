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
    let mut loops: Vec<(usize, usize)> = Vec::new();
    for (index, instruction) in function.instructions.iter().enumerate() {
        match instruction.operation {
            Operation::Loop(_) => loops.push((index, 1)),
            Operation::LoopValues(ref initials) => loops.push((index, initials.len())),
            Operation::While(_) => {
                if let Some(&(start, count)) = loops.last() {
                    for slot in start..start + count {
                        if let Some(last) = last_uses.get_mut(slot) {
                            *last = (*last).max(index);
                        }
                    }
                }
            }
            Operation::EndLoop(_) | Operation::EndLoopValues(_) => {
                if let Some((start, _)) = loops.pop() {
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

#[derive(Clone, PartialEq, Eq)]
struct Edge {
    target: usize,
    phi_inputs: BTreeSet<Register>,
}

struct Node {
    successors: Vec<Edge>,
    uses: BTreeSet<Register>,
    definitions: BTreeSet<Register>,
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
            definitions: BTreeSet::new(),
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
        else_node: Option<(usize, Vec<Register>)>,
    },
    Loop {
        header: usize,
        arity: usize,
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

    fn point(&mut self, index: usize, arity: usize) -> usize {
        let instruction = &self.function.instructions[index];
        let mut result = Node::new();
        let before_poll = matches!(
            instruction.operation,
            Operation::Loop(_)
                | Operation::LoopValues(_)
                | Operation::EndIf(_)
                | Operation::EndIfValues(_)
        );
        result.definitions = self.references(index..index + arity);
        result.defined_before_poll = before_poll;
        if !before_poll {
            result.uses = self.references(operands(&instruction.operation));
        }
        self.nodes[index] = result;
        index
    }

    fn virtual_node(&mut self, definitions: BTreeSet<Register>) -> usize {
        let node = self.nodes.len();
        let mut result = Node::new();
        result.definitions = definitions;
        result.defined_before_poll = true;
        self.nodes.push(result);
        node
    }

    fn connect(&mut self, source: usize, target: usize, phi_inputs: &[Register]) {
        let phi_inputs = self.references(phi_inputs.iter().copied());
        let edge = Edge { target, phi_inputs };
        if !self.nodes[source].successors.contains(&edge) {
            self.nodes[source].successors.push(edge);
        }
    }

    fn fallthrough(&mut self, source: Option<usize>, target: usize, phi_inputs: &[Register]) {
        if let Some(source) = source {
            self.connect(source, target, phi_inputs);
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
                Operation::PhiValue(_) => {}
                Operation::Loop(initial) => {
                    let header = self.point(index, 1);
                    self.fallthrough(current, header, &[initial]);
                    current = Some(header);
                    regions.push(Region::Loop {
                        header,
                        arity: 1,
                        while_node: None,
                    });
                }
                Operation::LoopValues(ref initials) => {
                    let arity = initials.len();
                    let header = self.point(index, arity);
                    self.fallthrough(current, header, initials);
                    current = Some(header);
                    regions.push(Region::Loop {
                        header,
                        arity,
                        while_node: None,
                    });
                }
                Operation::While(_) => {
                    let while_node = self.point(index, 1);
                    self.fallthrough(current, while_node, &[]);
                    current = Some(while_node);
                    if let Some(Region::Loop {
                        while_node: tested, ..
                    }) = regions.last_mut()
                    {
                        *tested = Some(while_node);
                    }
                }
                Operation::EndLoop(value) => {
                    let end = self.point(index, 1);
                    self.fallthrough(current, end, &[]);
                    let Some(Region::Loop {
                        header,
                        arity,
                        while_node,
                    }) = regions.pop()
                    else {
                        current = Some(end);
                        continue;
                    };
                    self.connect(end, header, &[value]);
                    let definitions = self.references(index..index + arity);
                    let after = self.virtual_node(definitions);
                    if let Some(while_node) = while_node {
                        let header_phis: Vec<Register> = (header..header + arity).collect();
                        self.connect(while_node, after, &header_phis);
                    }
                    current = Some(after);
                }
                Operation::EndLoopValues(ref values) => {
                    let arity = values.len();
                    let end = self.point(index, 0);
                    let mut node = Node::new();
                    node.uses = self.references(operands(operation));
                    self.nodes[index] = node;
                    self.fallthrough(current, end, &[]);
                    let Some(Region::Loop {
                        header,
                        arity: header_arity,
                        while_node,
                    }) = regions.pop()
                    else {
                        current = Some(end);
                        continue;
                    };
                    self.connect(end, header, values);
                    let definitions = self.references(index..index + arity);
                    let after = self.virtual_node(definitions);
                    if let Some(while_node) = while_node {
                        let header_phis: Vec<Register> = (header..header + header_arity).collect();
                        self.connect(while_node, after, &header_phis);
                    }
                    current = Some(after);
                }
                Operation::If(_) => {
                    let if_node = self.point(index, 1);
                    self.fallthrough(current, if_node, &[]);
                    current = Some(if_node);
                    regions.push(Region::Branch {
                        if_node,
                        else_node: None,
                    });
                }
                Operation::Else(value) => {
                    let else_node = self.point(index, 1);
                    self.fallthrough(current, else_node, &[]);
                    let after_else = self.virtual_node(BTreeSet::new());
                    if let Some(Region::Branch {
                        if_node,
                        else_node: slot,
                    }) = regions.last_mut()
                    {
                        self.connect(*if_node, after_else, &[]);
                        *slot = Some((else_node, vec![value]));
                    }
                    current = Some(after_else);
                }
                Operation::ElseValues(ref values) => {
                    let else_node = self.point(index, 1);
                    self.fallthrough(current, else_node, &[]);
                    let after_else = self.virtual_node(BTreeSet::new());
                    if let Some(Region::Branch {
                        if_node,
                        else_node: slot,
                    }) = regions.last_mut()
                    {
                        self.connect(*if_node, after_else, &[]);
                        *slot = Some((else_node, values.clone()));
                    }
                    current = Some(after_else);
                }
                Operation::EndIf(value) => {
                    let merge = self.point(index, 1);
                    self.fallthrough(current, merge, &[value]);
                    if let Some(Region::Branch {
                        else_node: Some((else_node, else_values)),
                        ..
                    }) = regions.pop()
                    {
                        self.connect(else_node, merge, &else_values);
                    }
                    current = Some(merge);
                }
                Operation::EndIfValues(ref values) => {
                    let merge = self.point(index, values.len());
                    self.fallthrough(current, merge, values);
                    if let Some(Region::Branch {
                        else_node: Some((else_node, else_values)),
                        ..
                    }) = regions.pop()
                    {
                        self.connect(else_node, merge, &else_values);
                    }
                    current = Some(merge);
                }
                _ => {
                    let point = self.point(index, 1);
                    self.fallthrough(current, point, &[]);
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
        self.fallthrough(current, final_node, &[]);
    }

    fn solve(&mut self) {
        loop {
            let mut changed = false;
            for index in (0..self.nodes.len()).rev() {
                let mut live_out = BTreeSet::new();
                for edge in &self.nodes[index].successors {
                    live_out.extend(self.nodes[edge.target].required_in.iter().copied());
                    live_out.extend(edge.phi_inputs.iter().copied());
                }

                let mut required_in = self.nodes[index].uses.clone();
                required_in.extend(live_out.iter().copied());
                for definition in &self.nodes[index].definitions {
                    required_in.remove(definition);
                }

                let mut at = required_in.clone();
                if self.nodes[index].defined_before_poll {
                    for definition in &self.nodes[index].definitions {
                        if live_out.contains(definition) {
                            at.insert(*definition);
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
