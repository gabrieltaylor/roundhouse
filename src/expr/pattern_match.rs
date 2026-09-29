use super::Expr;
use crate::{Symbol, span::Span};

#[derive(Clone, Debug)]
pub struct MatchPattern {
    pub span: Span,
    pub kind: MatchPatternKind,
}

#[derive(Clone, Debug)]
pub enum MatchPatternKind {
    Bind(Symbol),
    Value(Expr),
    Alternative(Box<MatchPattern>, Box<MatchPattern>),
    Capture(Box<MatchPattern>, Symbol),
    Array {
        constant: Option<Expr>,
        prefix: Vec<MatchPattern>,
        rest: Option<Option<Symbol>>,
        suffix: Vec<MatchPattern>,
    },
    Hash {
        constant: Option<Expr>,
        fields: Vec<(Expr, MatchPattern)>,
        rest: HashRest,
    },
}

#[derive(Clone, Debug)]
pub enum HashRest {
    Ignore,
    Any,
    Reject,
    Capture(Symbol),
}

pub struct MatchArm {
    pub pattern: MatchPattern,
    pub guard: Option<Expr>,
    pub body: Expr,
}
