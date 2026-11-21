use wings_source::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub text: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeExpr {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeKind {
    Named(Name),
    Unit,
    List(Box<TypeExpr>),
    ActorPid(Name),
    Error,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub structs: Vec<StructDecl>,
    pub actors: Vec<Actor>,
}

#[derive(Clone, Debug)]
pub struct Actor {
    pub name: Name,
    pub structs: Vec<StructDecl>,
    pub states: Vec<State>,
    pub handlers: Vec<Handler>,
    pub functions: Vec<Function>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct StructDecl {
    pub name: Name,
    pub fields: Vec<Parameter>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct State {
    pub name: Name,
    pub ty: Option<TypeExpr>,
    pub default: Expr,
}

#[derive(Clone, Debug)]
pub struct Parameter {
    pub name: Name,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug)]
pub struct Handler {
    pub is_reply: bool,
    pub name: Name,
    pub parameters: Vec<Parameter>,
    pub reply_type: Option<TypeExpr>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Function {
    pub name: Name,
    pub parameters: Vec<Parameter>,
    pub result: Option<TypeExpr>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub statements: Vec<Statement>,
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct AssignTarget {
    pub base: Name,
    pub fields: Vec<Name>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Statement {
    Let {
        name: Name,
        ty: Option<TypeExpr>,
        value: Expr,
    },
    Assign {
        target: AssignTarget,
        operator: Option<BinaryOperator>,
        value: Expr,
    },
    While {
        condition: Expr,
        body: Block,
    },
    ForRange {
        binding: Name,
        start: Expr,
        end: Expr,
        inclusive: bool,
        body: Block,
        span: Span,
    },
    ForList {
        binding: Name,
        list: Expr,
        body: Block,
        span: Span,
    },
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub enum InterpolatedPart {
    Literal(String),
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub enum MatchPattern {
    Wildcard(Span),
    Null(Span),
    Integer(i64, Span),
    Boolean(bool, Span),
    String(String, Span),
    EmptyList(Span),
    ConsList { head: Name, tail: Name, span: Span },
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Integer(i64),
    String(String),
    InterpolatedString(Vec<InterpolatedPart>),
    Bytes(Vec<u8>),
    Boolean(bool),
    Unit,
    Null,
    SelfPid,
    Spawn(Option<Name>),
    Name(Name),
    List {
        elements: Vec<Expr>,
        tail: Option<Box<Expr>>,
    },
    StructInit {
        name: Name,
        base: Option<Box<Expr>>,
        fields: Vec<(Name, Expr)>,
    },
    Field {
        value: Box<Expr>,
        name: Name,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: Name,
        arguments: Vec<Expr>,
    },
    StaticCall {
        namespace: Name,
        method: Name,
        arguments: Vec<Expr>,
    },
    Call {
        callee: Name,
        arguments: Vec<Expr>,
    },
    Send {
        target: Box<Expr>,
        message: Name,
        arguments: Vec<Expr>,
    },
    SendAfter {
        target: Box<Expr>,
        delay: Box<Expr>,
        message: Name,
        arguments: Vec<Expr>,
    },
    Monitor {
        target: Box<Expr>,
        handler: Name,
    },
    Ask {
        target: Box<Expr>,
        message: Name,
        arguments: Vec<Expr>,
        timeout: Box<Expr>,
        then: Name,
    },
    If {
        condition: Box<Expr>,
        then_branch: Block,
        else_branch: Option<Block>,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    Negate(Box<Expr>),
    Not(Box<Expr>),
    Binary {
        operator: BinaryOperator,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
}
