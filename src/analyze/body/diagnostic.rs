//! Analyze-time diagnostic detection.
//!
//! Runs after type inference populates `.ty` on an expression's
//! children; inspects the node shape + child types and, if the
//! body-typer recognizes a user error (an operator Ruby would raise
//! on — Incompatible `+`, cross-type comparison, …), annotates
//! `expr.diagnostic`. Downstream consumers:
//!
//! - Emitters check `.diagnostic` at the top of their expression
//!   walker; if set, they emit a target-side raise-equivalent
//!   instead of the normal rendering.
//! - `analyze::diagnose` collects annotations across the tree for
//!   CLI reporting.
//!
//! This module is the single site where analyze-time diagnostics
//! originate. New kinds (embedded conditionals, Hash `+`, etc.) hook
//! in here.

use crate::expr::{Expr, ExprNode};
use crate::ty::Ty;

pub(super) fn detect_diagnostic(expr: &mut Expr) {
    if let ExprNode::Send { recv: Some(r), method, args, .. } = &*expr.node {
        if expr.hint == Some(crate::expr::IrHint::PatternDeconstructOrigin) {
            if matches!(&expr.diagnostic, Some(crate::diagnostic::DiagnosticKind::Unsupported { construct, .. })
                if construct.as_str() == "pattern deconstruction") {
                expr.diagnostic = None;
            }
            fn unresolved_shape(ty: &Ty) -> bool {
                match ty {
                    Ty::Class { .. } => true,
                    Ty::Union { variants } => variants.iter().any(unresolved_shape),
                    _ => ty.is_unknown(),
                }
            }
            if expr.ty.as_ref().is_none_or(unresolved_shape) {
                expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                    target: None,
                    construct: crate::Symbol::from("pattern deconstruction"),
                    detail: format!("cannot statically resolve {} and its return shape; dynamic custom deconstruction is unsupported", method),
                });
            }
            return;
        }
        if args.len() != 1 {
            return;
        }
        let rhs = &args[0];
        let incompatible = match method.as_str() {
            "+" => {
                use crate::emit::shared::add::{AddCase, classify_add};
                matches!(classify_add(r, rhs), AddCase::Incompatible)
            }
            "-" => {
                use crate::emit::shared::sub::{SubCase, classify_sub};
                matches!(classify_sub(r, rhs), SubCase::Incompatible)
            }
            "*" => {
                use crate::emit::shared::mul::{MulCase, classify_mul};
                matches!(classify_mul(r, rhs), MulCase::Incompatible)
            }
            "/" | "**" => {
                use crate::emit::shared::div_pow::{DivPowCase, classify_div_pow};
                matches!(classify_div_pow(r, rhs), DivPowCase::Incompatible)
            }
            "%" => {
                use crate::emit::shared::modulo::{ModuloCase, classify_modulo};
                matches!(classify_modulo(r, rhs), ModuloCase::Incompatible)
            }
            "<" | "<=" | ">" | ">=" => {
                use crate::emit::shared::cmp::{CmpCase, classify_cmp};
                matches!(classify_cmp(r, rhs), CmpCase::Incompatible)
            }
            _ => false,
        };
        if incompatible {
            use crate::diagnostic::DiagnosticKind;
            let lhs_ty = r.ty.clone().unwrap_or(Ty::Nil);
            let rhs_ty = rhs.ty.clone().unwrap_or(Ty::Nil);
            expr.diagnostic = Some(DiagnosticKind::IncompatibleBinop {
                op: method.clone(),
                lhs_ty,
                rhs_ty,
            });
        }
    }
}
