use wings_source::Span;

#[derive(Clone, Debug)]
pub struct Name {
    pub text: String,
    pub span: Span,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub processes: Vec<Process>,
}

#[derive(Clone, Debug)]
pub struct Process {
    pub name: Name,
    pub states: Vec<State>,
    pub handlers: Vec<Handler>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct State {
    pub name: Name,
    pub ty: Name,
    pub default: Expr,
}

#[derive(Clone, Debug)]
pub struct Parameter {
    pub name: Name,
    pub ty: Name,
}

#[derive(Clone, Debug)]
pub struct Handler {
    pub name: Name,
    pub parameters: Vec<Parameter>,
    pub reply_type: Option<Name>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub statements: Vec<Statement>,
    pub tail: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Statement {
    Assign {
        target: Name,
        value: Expr,
    },
    Let {
        name: Name,
        ty: Option<Name>,
        value: Expr,
    },
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Integer(i64),
    Boolean(bool),
    Unit,
    Name(Name),
    Negate(Box<Expr>),
    Binary {
        operator: BinaryOperator,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Error,
}

#[derive(Clone, Copy, Debug)]
pub enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
}
