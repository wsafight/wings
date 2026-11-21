use std::collections::BTreeSet;

use super::{Function, Operation, Register, RootPlan, operands};

pub(super) fn build(function: &Function) -> RootPlan {
    let mut graph = Graph::new(function);
    graph.solve();
    graph.finish()
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
        let before_poll = matches!(instruction.operation, Operation::Loop(_) | Operation::EndIf(_));
        result.definitions = self.references(index..index + arity);
        result.defined_before_poll = before_poll;
        if !before_poll {
            result.uses = self.references(operands(&instruction.operation));
        }
        self.nodes[index] = result;
        index
    }

    fn virtual_node(&mut self, definitions: BTreeSet<Register>) -> usize {
        let index = self.nodes.len();
        let mut node = Node::new();
        node.definitions = definitions;
        self.nodes.push(node);
        index
    }

    fn connect(&mut self, from: usize, target: usize, phi_inputs: &[Register]) {
        let phi_inputs = self.references(phi_inputs.iter().copied());
        self.nodes[from].successors.push(Edge { target, phi_inputs });
    }

    fn fallthrough(&mut self, from: Option<usize>, target: usize, phi_inputs: &[Register]) {
        if let Some(from) = from {
            self.connect(from, target, phi_inputs);
        }
    }

    fn references(&self, registers: impl IntoIterator<Item = Register>) -> BTreeSet<Register> {
        registers
            .into_iter()
            .filter(|&register| self.is_reference(register))
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
                Operation::Loop(ref initials) => {
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
                Operation::EndLoop(ref values) => {
                    let arity = values.len();
                    let mut node = Node::new();
                    node.uses = self.references(operands(operation));
                    self.nodes[index] = node;
                    self.fallthrough(current, index, &[]);
                    let Some(Region::Loop {
                        header,
                        arity: header_arity,
                        while_node,
                    }) = regions.pop()
                    else {
                        current = Some(index);
                        continue;
                    };
                    self.connect(index, header, values);
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
                Operation::Else(ref values) => {
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
                Operation::EndIf(ref values) => {
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

    fn finish(self) -> RootPlan {
        let instruction_count = self.function.instructions.len();
        let mut at = Vec::with_capacity(instruction_count + 1);
        let mut capacity = 0;
        for node in &self.nodes[..=instruction_count] {
            capacity = capacity.max(node.at.len());
            at.push(node.at.iter().copied().collect());
        }
        RootPlan { capacity, at }
    }
}
