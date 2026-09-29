use crate::expr::pattern_match::{HashRest, MatchArm, MatchPattern, MatchPatternKind as P};
use crate::expr::{ArrayStyle, BoolOpKind, BoolOpSurface, Expr, ExprNode, LValue, Literal};
use crate::{Symbol, ident::VarId, span::Span};

pub fn lower(
    span: Span,
    subject: Expr,
    arms: Vec<MatchArm>,
    fallback: Option<Expr>,
    source: &str,
) -> Expr {
    let mut prefix = format!("__rh_pattern_{}_", span.start);
    while source.contains(&prefix) {
        prefix.push('_');
    }
    let mut builder = Matcher {
        span,
        prefix,
        next: 0,
        key_error: None,
    };
    let subject_name = builder.fresh();
    let subject_read = builder.var(&subject_name);
    let mut setup = vec![builder.assign(&subject_name, subject)];
    if fallback.is_none() && arms.len() == 1 {
        let error = builder.fresh();
        setup.push(builder.assign(&error, builder.lit(Literal::Nil)));
        builder.key_error = Some(error);
    }
    let mut branches = Vec::new();
    for arm in arms {
        let mut cond = builder.matches(arm.pattern, subject_read.clone());
        if let Some(guard) = arm.guard {
            cond = builder.and(cond, guard);
        }
        branches.push((cond, arm.body));
    }
    let mut tail = fallback.unwrap_or_else(|| {
        let message = builder.send(subject_read, "inspect", vec![]);
        let error = builder.send(
            builder.constant("NoMatchingPatternError"),
            "new",
            vec![message],
        );
        let error = match &builder.key_error {
            Some(name) => builder.or(builder.var(name), error),
            None => error,
        };
        builder.raise(error)
    });
    for (cond, then_branch) in branches.into_iter().rev() {
        tail = builder.expr(ExprNode::If {
            cond,
            then_branch,
            else_branch: tail,
        });
    }
    setup.push(tail);
    let mut result = builder.sequence(setup);
    result.hint = Some(crate::expr::IrHint::PatternMatchOrigin);
    result
}

struct Matcher {
    span: Span,
    prefix: String,
    next: usize,
    key_error: Option<Symbol>,
}

impl Matcher {
    fn expr(&self, node: ExprNode) -> Expr {
        Expr::new(self.span, node)
    }
    fn lit(&self, value: Literal) -> Expr {
        self.expr(ExprNode::Lit { value })
    }
    fn boolean(&self, value: bool) -> Expr {
        self.lit(Literal::Bool { value })
    }
    fn int(&self, value: usize) -> Expr {
        self.lit(Literal::Int {
            value: value as i64,
        })
    }
    fn constant(&self, name: &str) -> Expr {
        self.expr(ExprNode::Const {
            path: vec![Symbol::from(name)],
        })
    }
    fn fresh(&mut self) -> Symbol {
        self.next += 1;
        Symbol::from(format!("{}{}", self.prefix, self.next).as_str())
    }
    fn var(&self, name: &Symbol) -> Expr {
        self.expr(ExprNode::Var {
            id: VarId(0),
            name: name.clone(),
        })
    }
    fn assign(&self, name: &Symbol, value: Expr) -> Expr {
        self.expr(ExprNode::Assign {
            target: LValue::Var {
                id: VarId(0),
                name: name.clone(),
            },
            value,
        })
    }
    fn send(&self, recv: Expr, method: &str, args: Vec<Expr>) -> Expr {
        let parenthesized = !args.is_empty() && !matches!(method, "===" | "==" | ">=" | "-" | "!");
        self.expr(ExprNode::Send {
            recv: Some(recv),
            method: Symbol::from(method),
            args,
            block: None,
            parenthesized,
        })
    }
    fn raise(&self, value: Expr) -> Expr {
        self.expr(ExprNode::Send {
            recv: None,
            method: Symbol::from("raise"),
            args: vec![value],
            block: None,
            parenthesized: true,
        })
    }
    fn and(&self, left: Expr, right: Expr) -> Expr {
        if let ExprNode::BoolOp {
            op: BoolOpKind::And,
            left: inner,
            right: tail,
            ..
        } = &*right.node
        {
            return self.and(self.and(left, inner.clone()), tail.clone());
        }
        self.expr(ExprNode::BoolOp {
            op: BoolOpKind::And,
            surface: BoolOpSurface::Symbol,
            left,
            right,
        })
    }
    fn or(&self, left: Expr, right: Expr) -> Expr {
        if let ExprNode::BoolOp {
            op: BoolOpKind::Or,
            left: inner,
            right: tail,
            ..
        } = &*right.node
        {
            return self.or(self.or(left, inner.clone()), tail.clone());
        }
        self.expr(ExprNode::BoolOp {
            op: BoolOpKind::Or,
            surface: BoolOpSurface::Symbol,
            left,
            right,
        })
    }
    fn sequence(&self, exprs: Vec<Expr>) -> Expr {
        let body = self.expr(ExprNode::Seq { exprs });
        self.expr(ExprNode::BeginRescue {
            body,
            rescues: vec![],
            else_branch: None,
            ensure: None,
            implicit: false,
        })
    }
    fn bind(&self, name: &Symbol, value: Expr) -> Expr {
        self.sequence(vec![self.assign(name, value), self.boolean(true)])
    }
    fn deconstruct(&self, value: Expr, name: &Symbol, hash: bool, args: Vec<Expr>) -> Expr {
        let method = if hash {
            "deconstruct_keys"
        } else {
            "deconstruct"
        };
        let class = if hash { "Hash" } else { "Array" };
        let responds = self.send(
            value.clone(),
            "respond_to?",
            vec![self.lit(Literal::Sym {
                value: Symbol::from(method),
            })],
        );
        let mut call = self.send(value, method, args);
        call.hint = Some(crate::expr::IrHint::PatternDeconstructOrigin);
        let check = self.send(self.var(name), "is_a?", vec![self.constant(class)]);
        let error = self.send(
            self.constant("TypeError"),
            "new",
            vec![self.lit(Literal::Str {
                value: format!("{method} must return {class}"),
            })],
        );
        let checked = self.expr(ExprNode::If {
            cond: check,
            then_branch: self.boolean(true),
            else_branch: self.raise(error),
        });
        self.and(
            responds,
            self.sequence(vec![self.assign(name, call), checked]),
        )
    }
    fn field(&mut self, pattern: MatchPattern, mut read: Expr, discard: Option<Expr>) -> Expr {
        read.hint = Some(crate::expr::IrHint::PatternCheckedRead);
        let name = self.fresh();
        let condition = self.matches(pattern, self.var(&name));
        let mut steps = vec![self.assign(&name, read)];
        steps.extend(discard);
        steps.push(condition);
        self.sequence(steps)
    }
    fn matches(&mut self, pattern: MatchPattern, value: Expr) -> Expr {
        let saved = self.span;
        self.span = pattern.span;
        let result = match pattern.kind {
            P::Bind(name) => self.bind(&name, value),
            P::Value(pattern) => self.send(pattern, "===", vec![value]),
            P::Alternative(left, right) => {
                let left = self.matches(*left, value.clone());
                let right = self.matches(*right, value);
                self.or(left, right)
            }
            P::Capture(pattern, name) => {
                let test = self.matches(*pattern, value.clone());
                self.and(test, self.bind(&name, value))
            }
            P::Array {
                constant,
                prefix,
                rest,
                suffix,
            } => {
                let name = self.fresh();
                let array = self.var(&name);
                let mut cond = self.deconstruct(value.clone(), &name, false, vec![]);
                if let Some(class) = constant {
                    cond = self.and(self.send(class, "===", vec![value]), cond);
                }
                let size = self.send(array.clone(), "length", vec![]);
                let len = prefix.len() + suffix.len();
                cond = self.and(
                    cond,
                    self.send(
                        size.clone(),
                        if rest.is_some() { ">=" } else { "==" },
                        vec![self.int(len)],
                    ),
                );
                let prefix_len = prefix.len();
                let suffix_len = suffix.len();
                for (i, pattern) in prefix.into_iter().enumerate() {
                    let read = self.send(array.clone(), "[]", vec![self.int(i)]);
                    let test = self.field(pattern, read, None);
                    cond = self.and(cond, test);
                }
                if let Some(Some(name)) = rest {
                    let count = self.send(size.clone(), "-", vec![self.int(len)]);
                    let tail = self.send(array.clone(), "drop", vec![self.int(prefix_len)]);
                    let rest = self.send(tail, "take", vec![count]);
                    cond = self.and(cond, self.bind(&name, rest));
                }
                for (i, pattern) in suffix.into_iter().enumerate() {
                    let index = self.lit(Literal::Int {
                        value: -((suffix_len - i) as i64),
                    });
                    let read = self.send(array.clone(), "[]", vec![index]);
                    let test = self.field(pattern, read, None);
                    cond = self.and(cond, test);
                }
                cond
            }
            P::Hash {
                constant,
                fields,
                rest,
            } => {
                let name = self.fresh();
                let hash = self.var(&name);
                let keys = match &rest {
                    HashRest::Capture(_) | HashRest::Reject => self.lit(Literal::Nil),
                    HashRest::Ignore if fields.is_empty() => self.lit(Literal::Nil),
                    _ => self.expr(ExprNode::Array {
                        elements: fields.iter().map(|(k, _)| k.clone()).collect(),
                        style: ArrayStyle::Brackets,
                    }),
                };
                let mut cond = self.deconstruct(value.clone(), &name, true, vec![keys]);
                if let Some(class) = constant {
                    cond = self.and(self.send(class, "===", vec![value]), cond);
                }
                for (key, _) in &fields {
                    let mut present = self.send(hash.clone(), "key?", vec![key.clone()]);
                    if let Some(error_name) = &self.key_error {
                        let kwargs = self.expr(ExprNode::Hash {
                            entries: vec![
                                (
                                    self.lit(Literal::Sym {
                                        value: Symbol::from("matchee"),
                                    }),
                                    hash.clone(),
                                ),
                                (
                                    self.lit(Literal::Sym {
                                        value: Symbol::from("key"),
                                    }),
                                    key.clone(),
                                ),
                            ],
                            kwargs: true,
                        });
                        let error = self.send(
                            self.constant("NoMatchingPatternKeyError"),
                            "new",
                            vec![
                                self.lit(Literal::Str {
                                    value: "key not found".into(),
                                }),
                                kwargs,
                            ],
                        );
                        present = self.or(
                            present,
                            self.sequence(vec![
                                self.assign(error_name, error),
                                self.boolean(false),
                            ]),
                        );
                    }
                    cond = self.and(cond, present);
                }
                if fields.is_empty() && matches!(rest, HashRest::Ignore) {
                    cond = self.and(cond, self.send(hash.clone(), "empty?", vec![]));
                }
                let mut remaining = None;
                if matches!(rest, HashRest::Capture(_) | HashRest::Reject) {
                    let copy = self.fresh();
                    let duplicate = self.send(hash.clone(), "dup", vec![]);
                    cond = self.and(cond, self.bind(&copy, duplicate));
                    remaining = Some(self.var(&copy));
                }
                for (key, pattern) in fields {
                    let discard = remaining
                        .as_ref()
                        .map(|h| self.send(h.clone(), "delete", vec![key.clone()]));
                    let read = self.send(
                        remaining.clone().unwrap_or_else(|| hash.clone()),
                        "[]",
                        vec![key],
                    );
                    let test = self.field(pattern, read, discard);
                    cond = self.and(cond, test);
                }
                match rest {
                    HashRest::Reject => {
                        cond = self.and(cond, self.send(remaining.unwrap(), "empty?", vec![]))
                    }
                    HashRest::Capture(name) => {
                        cond = self.and(cond, self.bind(&name, remaining.unwrap()))
                    }
                    _ => {}
                }
                cond
            }
        };
        let result = if let Some(name) = &self.key_error {
            self.sequence(vec![self.assign(name, self.lit(Literal::Nil)), result])
        } else {
            result
        };
        self.span = saved;
        result
    }
}

pub fn apply_protocol_lowering(app: &mut crate::App) {
    super::for_each_hook_body(app, &mut lower_protocols);
}

pub fn lower_protocols(expr: &mut Expr) {
    use crate::ty::Ty;
    expr.node.for_each_child_mut(&mut lower_protocols);
    let ExprNode::Send {
        recv: Some(recv),
        method,
        args,
        block: None,
        ..
    } = &*expr.node
    else {
        return;
    };
    let array = matches!(recv.ty, Some(Ty::Array { .. } | Ty::Tuple { .. }));
    let hash = matches!(recv.ty, Some(Ty::Hash { .. } | Ty::Record { .. }));
    if (method.as_str() == "deconstruct" && array && args.is_empty())
        || (method.as_str() == "deconstruct_keys"
            && hash
            && args.len() == 1
            && protocol_keys(&args[0]))
    {
        *expr = recv.clone();
    } else if method.as_str() == "respond_to?"
        && args.len() == 1
        && matches!(&*recv.node, ExprNode::Var { .. })
    {
        if let ExprNode::Lit {
            value: Literal::Sym { value },
        } = &*args[0].node
        {
            let answer = match value.as_str() {
                "deconstruct" if array || hash => Some(array),
                "deconstruct_keys" if array || hash => Some(hash),
                _ => None,
            };
            if let Some(value) = answer {
                *expr.node = ExprNode::Lit {
                    value: Literal::Bool { value },
                };
            }
        }
    }
}

fn protocol_keys(expr: &Expr) -> bool {
    match &*expr.node {
        ExprNode::Lit {
            value: Literal::Nil,
        } => true,
        ExprNode::Array { elements, .. } => elements
            .iter()
            .all(|e| matches!(&*e.node, ExprNode::Lit { .. })),
        _ => false,
    }
}

pub fn check_target(app: &crate::App, target: crate::project::BuildTarget) -> Result<(), String> {
    use crate::project::BuildTarget;
    if matches!(
        target,
        BuildTarget::Ruby | BuildTarget::Jruby | BuildTarget::Blog | BuildTarget::Roda
    ) {
        return Ok(());
    }
    fn visit(expr: &Expr, spans: &mut Vec<Span>) {
        if expr.hint == Some(crate::expr::IrHint::PatternMatchOrigin) {
            spans.push(expr.span);
        }
        expr.node.for_each_child(&mut |e| visit(e, spans));
    }
    let mut spans = Vec::new();
    super::for_each_hook_body_ref(app, &mut |e| visit(e, &mut spans));
    if spans.is_empty() {
        return Ok(());
    }
    for span in spans {
        crate::emit::diagnostics::push(crate::diagnostic::Diagnostic::unsupported(
            span,
            Some(Symbol::from(target.as_str())),
            Symbol::from("case/in pattern matching"),
            "this target does not yet preserve pattern-local scope and Ruby case equality",
        ));
    }
    Err(format!(
        "case/in pattern matching is not supported by the {} target; Ruby and JRuby output are supported",
        target.as_str()
    ))
}

pub(crate) fn absent_protocol(ty: Option<&crate::ty::Ty>, method: &str) -> bool {
    use crate::ty::Ty;
    match ty {
        Some(Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Sym | Ty::Nil) => true,
        Some(Ty::Array { .. } | Ty::Tuple { .. }) => method == "deconstruct_keys",
        Some(Ty::Hash { .. } | Ty::Record { .. }) => method == "deconstruct",
        _ => false,
    }
}
