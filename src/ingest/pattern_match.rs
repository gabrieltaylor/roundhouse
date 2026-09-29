use ruby_prism::Node;

use crate::expr::pattern_match::{HashRest, MatchArm, MatchPattern, MatchPatternKind as P};
use crate::expr::{Expr, ExprNode, Literal};
use crate::{Symbol, span::Span};

use super::expr::ingest_expr;
use super::util::constant_id_str;
use super::{IngestError, IngestResult};

fn span(node: &Node<'_>, file: &str) -> Span {
    let loc = node.location();
    Span {
        file: super::sources::file_id(file),
        start: loc.start_offset() as u32,
        end: loc.end_offset() as u32,
    }
}

fn unsupported(node: &Node<'_>, file: &str, detail: &str) -> IngestError {
    let loc = node.location();
    IngestError::Unsupported {
        file: file.into(),
        message: format!(
            "case/in pattern at bytes {}..{}: {detail}: {}",
            loc.start_offset(),
            loc.end_offset(),
            String::from_utf8_lossy(loc.as_slice())
        ),
    }
}

fn binding(node: &Node<'_>, file: &str) -> IngestResult<Symbol> {
    node.as_local_variable_target_node()
        .map(|n| Symbol::from(constant_id_str(&n.name())))
        .ok_or_else(|| unsupported(node, file, "expected a local binding"))
}

fn pattern(node: &Node<'_>, file: &str) -> IngestResult<MatchPattern> {
    let kind = if let Some(n) = node.as_parentheses_node() {
        let inner = n
            .body()
            .ok_or_else(|| unsupported(node, file, "empty pattern"))?;
        let inner = inner
            .as_statements_node()
            .and_then(|s| s.body().iter().next())
            .unwrap_or(inner);
        return pattern(&inner, file);
    } else if let Some(n) = node.as_implicit_node() {
        return pattern(&n.value(), file);
    } else if node.as_local_variable_target_node().is_some() {
        P::Bind(binding(node, file)?)
    } else if let Some(n) = node.as_alternation_pattern_node() {
        P::Alternative(
            Box::new(pattern(&n.left(), file)?),
            Box::new(pattern(&n.right(), file)?),
        )
    } else if let Some(n) = node.as_capture_pattern_node() {
        P::Capture(
            Box::new(pattern(&n.value(), file)?),
            binding(&n.target().as_node(), file)?,
        )
    } else if let Some(n) = node.as_pinned_variable_node() {
        P::Value(ingest_expr(&n.variable(), file)?)
    } else if let Some(n) = node.as_pinned_expression_node() {
        P::Value(ingest_expr(&n.expression(), file)?)
    } else if let Some(n) = node.as_array_pattern_node() {
        let rest = n
            .rest()
            .map(|r| {
                let s = r
                    .as_splat_node()
                    .ok_or_else(|| unsupported(&r, file, "array rest"))?;
                s.expression().map(|v| binding(&v, file)).transpose()
            })
            .transpose()?;
        P::Array {
            constant: n.constant().map(|c| ingest_expr(&c, file)).transpose()?,
            prefix: n
                .requireds()
                .iter()
                .map(|p| pattern(&p, file))
                .collect::<IngestResult<_>>()?,
            rest,
            suffix: n
                .posts()
                .iter()
                .map(|p| pattern(&p, file))
                .collect::<IngestResult<_>>()?,
        }
    } else if let Some(n) = node.as_hash_pattern_node() {
        let rest = match n.rest() {
            None => HashRest::Ignore,
            Some(r) if r.as_no_keywords_parameter_node().is_some() => HashRest::Reject,
            Some(r) => {
                let s = r
                    .as_assoc_splat_node()
                    .ok_or_else(|| unsupported(&r, file, "hash rest"))?;
                match s.value() {
                    Some(v) => HashRest::Capture(binding(&v, file)?),
                    None => HashRest::Any,
                }
            }
        };
        let mut fields = Vec::new();
        for field in n.elements().iter() {
            let a = field
                .as_assoc_node()
                .ok_or_else(|| unsupported(&field, file, "hash field"))?;
            fields.push((ingest_expr(&a.key(), file)?, pattern(&a.value(), file)?));
        }
        P::Hash {
            constant: n.constant().map(|c| ingest_expr(&c, file)).transpose()?,
            fields,
            rest,
        }
    } else if node.as_find_pattern_node().is_some() {
        return Err(unsupported(node, file, "find patterns are not supported"));
    } else {
        let value = super::expr::ingest_expr_strict(node, file)
            .map_err(|_| unsupported(node, file, "unsupported value pattern"))?;
        P::Value(value)
    };
    Ok(MatchPattern {
        span: span(node, file),
        kind,
    })
}

fn body(node: Option<ruby_prism::StatementsNode<'_>>, file: &str) -> IngestResult<Expr> {
    node.map(|n| ingest_expr(&n.as_node(), file))
        .unwrap_or_else(|| {
            Ok(Expr::new(
                Span::synthetic(),
                ExprNode::Lit {
                    value: Literal::Nil,
                },
            ))
        })
}

pub(super) fn ingest_case(node: &ruby_prism::CaseMatchNode<'_>, file: &str) -> IngestResult<Expr> {
    let source = node.as_node();
    let subject = node
        .predicate()
        .ok_or_else(|| unsupported(&source, file, "missing subject"))?;
    let subject = ingest_expr(&subject, file)?;
    let mut arms = Vec::new();
    for branch in node.conditions().iter() {
        let n = branch
            .as_in_node()
            .ok_or_else(|| unsupported(&branch, file, "expected in branch"))?;
        let mut p = n.pattern();
        let guard = if let Some(g) = p.as_if_node() {
            let guard = ingest_expr(&g.predicate(), file)?;
            p = g
                .statements()
                .and_then(|s| s.body().iter().next())
                .ok_or_else(|| unsupported(&p, file, "empty guard pattern"))?;
            Some(guard)
        } else if let Some(g) = p.as_unless_node() {
            let predicate = ingest_expr(&g.predicate(), file)?;
            let guard = Expr::new(
                span(&p, file),
                ExprNode::Send {
                    recv: Some(predicate),
                    method: Symbol::from("!"),
                    args: vec![],
                    block: None,
                    parenthesized: false,
                },
            );
            p = g
                .statements()
                .and_then(|s| s.body().iter().next())
                .ok_or_else(|| unsupported(&p, file, "empty guard pattern"))?;
            Some(guard)
        } else {
            None
        };
        arms.push(MatchArm {
            pattern: pattern(&p, file)?,
            guard,
            body: body(n.statements(), file)?,
        });
    }
    let fallback = node
        .else_clause()
        .map(|e| body(e.statements(), file))
        .transpose()?;
    let reserved = super::sources::text_of(file)
        .unwrap_or_else(|| String::from_utf8_lossy(source.location().as_slice()).into_owned());
    Ok(crate::lower::pattern_match::lower(
        span(&source, file),
        subject,
        arms,
        fallback,
        &reserved,
    ))
}
