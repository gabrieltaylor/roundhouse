// Not left in one hash: `scope_chain` renders a Range condition as SQL only as a hash's sole entry, and a Range beside other keys matched nothing.
use crate::app::App;
use crate::diagnostic::Diagnostic;
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;

pub fn apply_where_range_split(app: &mut App) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    super::for_each_hook_body(app, &mut |body| {
        rewrite(body);
        ledger_stray_time_ranges(body, false, &mut diags);
    });
    for view in &mut app.views {
        rewrite(&mut view.body);
        ledger_stray_time_ranges(&mut view.body, false, &mut diags);
    }
    diags
}

// Not left silent: spinel has no Range of Time, so `t.all_month` anywhere but a `where`/`find_by` condition value cannot compile.
fn ledger_stray_time_ranges(expr: &mut Expr, as_condition: bool, diags: &mut Vec<Diagnostic>) {
    if !as_condition && is_all_range(expr) {
        diags.push(super::residue_diagnostic(
            "where_range_split",
            "time_range",
            expr.span,
            "no_time_range_at_runtime",
            "`all_day`/`all_week`/`all_month`/`all_year` grounds only as a `where`/`find_by` condition \
             value; elsewhere it needs a Range of Time, which spinel does not have"
                .to_string(),
        ));
    }
    let condition_hash = matches!(&*expr.node,
        ExprNode::Send { method, args, .. }
            if matches!(method.as_str(), "where" | "find_by" | "find_by!")
                && matches!(args.as_slice(), [a] if matches!(&*a.node, ExprNode::Hash { .. })));
    if condition_hash {
        if let ExprNode::Send {
            recv, args, block, ..
        } = &mut *expr.node
        {
            if let Some(r) = recv {
                ledger_stray_time_ranges(r, false, diags);
            }
            if let ExprNode::Hash { entries, .. } = &mut *args[0].node {
                for (k, v) in entries.iter_mut() {
                    ledger_stray_time_ranges(k, false, diags);
                    ledger_stray_time_ranges(v, true, diags);
                }
            }
            if let Some(b) = block {
                ledger_stray_time_ranges(b, false, diags);
            }
        }
        return;
    }
    expr.node
        .for_each_child_mut(&mut |c| ledger_stray_time_ranges(c, false, diags));
}

fn is_all_range(e: &Expr) -> bool {
    let ExprNode::Range { begin: Some(b), .. } = &*e.node else {
        return false;
    };
    matches!(&*b.node,
        ExprNode::Send { recv: Some(r), method, .. }
            if matches!(&*r.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == "ActiveSupport")
                && method.as_str().starts_with("beginning_of_"))
}

fn rewrite(expr: &mut Expr) {
    expr.node.for_each_child_mut(&mut rewrite);
    let ExprNode::Send {
        recv: Some(rel),
        method,
        args,
        block: None,
        ..
    } = &mut *expr.node
    else {
        return;
    };
    if !matches!(method.as_str(), "where" | "find_by" | "find_by!") {
        return;
    }
    let [arg] = &mut args[..] else { return };
    let ExprNode::Hash { entries, kwargs } = &mut *arg.node else {
        return;
    };
    if entries.len() < 2 || !entries.iter().any(|(_, v)| is_range(v)) {
        return;
    }
    let Some(model) = model_of(rel.ty.as_ref()) else {
        return;
    };
    let (mut split, rest): (Vec<_>, Vec<_>) = std::mem::take(entries)
        .into_iter()
        .partition(|(_, v)| is_range(v));
    *entries = if rest.is_empty() {
        vec![split.pop().expect("a range entry")]
    } else {
        rest
    };
    let mut chain = rel.clone();
    for entry in split {
        let span = chain.span;
        let hash = Expr::new(
            span,
            ExprNode::Hash {
                entries: vec![entry],
                kwargs: *kwargs,
            },
        );
        let mut next = Expr::new(
            span,
            ExprNode::Send {
                recv: Some(chain),
                method: Symbol::from("where"),
                args: vec![hash],
                block: None,
                parenthesized: true,
            },
        );
        next.ty = Some(Ty::Relation { of: model.clone() });
        chain = next;
    }
    *rel = chain;
}

fn is_range(e: &Expr) -> bool {
    matches!(&*e.node, ExprNode::Range { .. })
}

fn model_of(ty: Option<&Ty>) -> Option<ClassId> {
    match ty? {
        Ty::Relation { of } => Some(of.clone()),
        Ty::Class { id, args } if args.is_empty() => Some(id.clone()),
        _ => None,
    }
}
