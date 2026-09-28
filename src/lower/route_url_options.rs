//! `x_path(…, host: "example.com")` → `x_path(…)`.
//!
//! Rails partitions a route helper's option hash into the keys
//! `url_for` consumes ITSELF and the keys it forwards to the path
//! generator. `ActionDispatch::Routing::RouteSet::RESERVED_OPTIONS`
//! (actionpack 8.1.3, `route_set.rb:838`) is that first list:
//!
//! ```text
//! [:host, :protocol, :port, :subdomain, :domain, :tld_length,
//!  :trailing_slash, :anchor, :params, :only_path, :script_name,
//!  :original_script_name]
//! ```
//!
//! `url_for` deletes every one of them from `path_options` before
//! generating (`route_set.rb:864`), so none can ever reach the query
//! string. We forwarded them all, and a leftover option is a query key
//! here: campfire's `room_at_message_url(@room, msg, host:
//! "once.campfire.test")` rendered `/rooms/1/@5?host=once.campfire.test`
//! where Rails renders `http://once.campfire.test/rooms/1/@5`. That is
//! the app's own broadcast assertion failing, and note it is wrong
//! TWICE — a query key that should not be there, and an authority that
//! should. Dropping `host:` alone fixes only the first half and leaves
//! a bare path being compared against an absolute URL.
//!
//! **A rule table, not a special case for `host:`.** The list splits
//! three ways once you ask what each key does to a PATH:
//!
//!   * [`HOST_ONLY_OPTIONS`] — the seven that only describe the host
//!     half of a URL. `path_for` renders no host at all, so dropping
//!     them is EXACT rather than an approximation. This pass strips
//!     them from the call site; the demand survey in
//!     `routes_to_library` refuses to make a parameter out of them.
//!
//!     **On the `_url` spelling they are not dropped — they are the
//!     answer.** `x_url(…, host: h)` renders `"http://#{h}#{x_path(…)}"`,
//!     the same shape the view lowerer grounds a hostless `_url` with
//!     (`Rails.application.domain` in place of `h`) and the same one
//!     `emit::ruby::library::rewrite_url_helpers_absolute` builds for
//!     the explicit `…routes.url_helpers.x_url(…, host:)` chain.
//!     Dropping the host here would be the second half of the campfire
//!     bug rather than its fix: the button that assertion compares
//!     against holds an ABSOLUTE URL.
//!   * `anchor:` — the fragment. It DOES belong on a path, and
//!     `routes_to_library` now renders it (`#tag`, after the query
//!     string, exactly where `path_for` puts it). Four lobsters call
//!     sites and two writebook ones were passing it to a helper that
//!     had no such parameter.
//!   * `format:` — owned by [`super::route_format_suffix`], which
//!     monomorphizes the helper instead of widening its signature.
//!
//!   * `params:` — the query itself. `url_for` merges its value into the
//!     generated query string, which is what an erased `**splat` renders
//!     too, so `rewrites::route_helper_query_splat_index` reads the two
//!     as one shape: `x_path(…) + RouteHelpers.query_suffix(h)`. campfire's
//!     tests write it twice, both with a nested value.
//!
//! The three left over — `script_name:`, `original_script_name:` and
//! `trailing_slash:` — each genuinely change the path, and none is
//! modeled. They stay query keys, which is visibly wrong rather than
//! silently wrong, and no corpus app writes one on an app route.
//! Ledgered in `docs/pipeline/runtime.md`.
//!
//! Scope, and ordering: bare or explicit `RouteHelpers` calls whose name
//! matches a route in the app's own table, rewritten before
//! `lower_routes_to_library_functions` surveys those same call sites.
//! `rails_blob_path(…, only_path: true)` is an ActiveStorage helper
//! with its own declared parameter and names no app route, so it is
//! untouched.

use crate::app::App;
use crate::expr::{Expr, ExprNode, InterpPart, Literal};
use crate::ident::Symbol;

/// The `RESERVED_OPTIONS` entries that describe only the HOST half of
/// a URL — protocol, authority, and the choice of whether to render
/// one at all.
///
/// `path_for` is `url_for(…, PATH, …)`, and the `PATH` strategy
/// returns `options[:path]` with the script name, params and anchor
/// applied and nothing else (`http/url.rb`). Every key here feeds
/// `build_host_url`, which that strategy never calls. So a `_path`
/// helper drops them, and drops them exactly.
pub(crate) const HOST_ONLY_OPTIONS: &[&str] = &[
    "host",
    "protocol",
    "port",
    "subdomain",
    "domain",
    "tld_length",
    "only_path",
];

pub fn apply_route_url_options_lowering(app: &mut App) {
    let params = helper_path_params(app);
    let helpers = params.keys().cloned().collect::<std::collections::HashSet<_>>();
    if helpers.is_empty() {
        return;
    }
    super::for_each_hook_body(app, &mut |e| position_path_params(e, &params));
    for view in &mut app.views {
        position_path_params(&mut view.body, &params);
    }
    super::for_each_hook_body(app, &mut |e| rewrite(e, &helpers));
    for view in &mut app.views {
        rewrite(&mut view.body, &helpers);
    }
    for helper in &mut app.routes.direct_helpers {
        position_path_params(&mut helper.body, &params);
        rewrite(&mut helper.body, &helpers);
    }
    // `for_each_hook_body` does not reach test bodies, and the call
    // site this pass exists for is one — campfire's
    // `MessagesControllerTest`.
    for tm in &mut app.test_modules {
        if let Some(setup) = &mut tm.setup {
            rewrite(setup, &helpers);
        }
        for t in &mut tm.tests {
            rewrite(&mut t.body, &helpers);
        }
        for m in &mut tm.helpers {
            rewrite(&mut m.body, &helpers);
        }
    }
}

/// Each named helper's path params, in slot order. A name several
/// flattened routes share (lobsters' `/s/:id/(:title)` flattens to
/// `/s/:id/:title` and `/s/:id`) answers its longest list — the helper
/// the generator builds takes every slot.
fn helper_path_params(app: &App) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = Default::default();
    for route in super::routes::route_helpers(app) {
        for suffix in ["_path", "_url"] {
            let slot = out.entry(format!("{}{suffix}", route.as_name)).or_default();
            if route.path_params.len() > slot.len() {
                *slot = route.path_params.clone();
            }
        }
    }
    let declared = out.keys().filter(|name| name.ends_with("_path")).cloned().collect();
    for (variant, (base, _)) in super::routes_to_library::format_variant_demand(app, &declared) {
        if let Some(params) = out.get(&format!("{base}_path")).cloned() {
            let stem = variant.strip_suffix("_path").expect("format variant path helper");
            out.insert(format!("{stem}_url"), params.clone());
            out.insert(variant, params);
        }
    }
    out
}

/// `story_short_id_path(id, title: slug)` → `story_short_id_path(id,
/// slug)`. Rails fills a path param from the option hash as readily as
/// from a positional argument — lobsters names the optional `(:title)`
/// segment on every story link — but the generated helper takes path
/// params positionally, and the keyword reached it as an unknown one.
/// A keyword naming a path param moves into that param's slot; a slot
/// skipped on the way (an optional one nothing names) is filled with
/// `nil`, which is what the helper's own default would have been.
fn position_path_params(
    expr: &mut Expr,
    params: &std::collections::HashMap<String, Vec<String>>,
) {
    expr.node.for_each_child_mut(&mut |c| position_path_params(c, params));
    let span = expr.span;
    let ExprNode::Send { recv, method, args, block: None, .. } = &mut *expr.node else {
        return;
    };
    if !super::route_helper_receiver::is_helper_receiver(recv) {
        return;
    }
    let Some(names) = params.get(method.as_str()) else { return };
    let Some(last) = args.last() else { return };
    if let Some(spread) = spread_options_local(last, args.len() - 1, names) {
        args.pop();
        args.extend(spread);
        return;
    }
    let ExprNode::Hash { entries, kwargs: true } = &*last.node else { return };
    let key_of = |k: &Expr| match &*k.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
        _ => None,
    };
    let positional = args.len() - 1;
    let named: Vec<usize> = (positional..names.len())
        .filter(|i| entries.iter().any(|(k, _)| key_of(k).as_deref() == Some(names[*i].as_str())))
        .collect();
    let Some(&furthest) = named.last() else { return };
    let Some(last) = args.pop() else { return };
    let ExprNode::Hash { mut entries, .. } = *last.node else { unreachable!() };
    for i in positional..=furthest {
        let taken = entries
            .iter()
            .position(|(k, _)| key_of(k).as_deref() == Some(names[i].as_str()))
            .map(|at| entries.remove(at).1);
        args.push(taken.unwrap_or_else(|| {
            Expr::new(span, ExprNode::Lit { value: Literal::Nil })
        }));
    }
    if !entries.is_empty() {
        args.push(Expr::new(last.span, ExprNode::Hash { entries, kwargs: true }));
    }
}

/// `story_short_id_path(story, options)` where `options` is a local
/// Hash — lobsters' `comment_target_path` builds `{anchor: …}` and
/// adds `title:` conditionally. Rails reads a trailing Hash as the
/// option hash; the generated helper, with nothing to tell it so, bound
/// the whole Hash to the `title` slot. Spread into the path slots the
/// call left open plus `anchor:` — the keys this lowering knows the
/// helper to take. `None` unless the argument is a Hash-typed local
/// sitting IN a path slot (a local, so reading it once per key is
/// free of effects).
fn spread_options_local(arg: &Expr, index: usize, names: &[String]) -> Option<Vec<Expr>> {
    if index >= names.len() || index == 0 {
        return None;
    }
    let ExprNode::Var { .. } = &*arg.node else { return None };
    let Some(crate::ty::Ty::Hash { value, .. }) = &arg.ty else { return None };
    let read = |key: &str| {
        let mut e = Expr::new(
            arg.span,
            ExprNode::Send {
                recv: Some(arg.clone()),
                method: Symbol::from("[]"),
                args: vec![Expr::new(
                    arg.span,
                    ExprNode::Lit { value: Literal::Sym { value: Symbol::from(key) } },
                )],
                block: None,
                parenthesized: true,
            },
        );
        e.ty = Some(crate::ty::Ty::Union { variants: vec![(**value).clone(), crate::ty::Ty::Nil] });
        e
    };
    let mut out: Vec<Expr> = names[index..].iter().map(|n| read(n)).collect();
    let anchor_key = Expr::new(
        arg.span,
        ExprNode::Lit { value: Literal::Sym { value: Symbol::from("anchor") } },
    );
    out.push(Expr::new(
        arg.span,
        ExprNode::Hash { entries: vec![(anchor_key, read("anchor"))], kwargs: true },
    ));
    Some(out)
}

fn rewrite(expr: &mut Expr, helpers: &std::collections::HashSet<String>) {
    expr.node.for_each_child_mut(&mut |c| rewrite(c, helpers));
    ground_symbol_query_values(expr, helpers);
    let Some((stem, host, protocol)) = strip_host_options(expr, helpers) else {
        return;
    };
    let span = expr.span;
    let node = std::mem::replace(&mut *expr.node, ExprNode::Seq { exprs: vec![] });
    let ExprNode::Send { recv, args, parenthesized, .. } = node else { unreachable!() };
    let mut path_call = Expr::new(
        span,
        ExprNode::Send {
            recv,
            method: Symbol::from(format!("{stem}_path")),
            args,
            block: None,
            parenthesized,
        },
    );
    path_call.ty = expr.ty.clone().or(Some(crate::ty::Ty::Str));
    // `protocol:` rides bare (`"https"`), the convention
    // `rewrite_url_helpers_absolute` already set — Rails'
    // `normalize_protocol` accepts `"https"` and `"https://"` alike and
    // we accept only the first.
    let mut parts: Vec<InterpPart> = Vec::new();
    match protocol {
        Some(p) => parts.push(InterpPart::Expr { expr: p }),
        None => parts.push(InterpPart::Text { value: "http".to_string() }),
    }
    parts.push(InterpPart::Text { value: "://".to_string() });
    parts.push(InterpPart::Expr { expr: host });
    parts.push(InterpPart::Expr { expr: path_call });
    *expr.node = ExprNode::StringInterp { parts };
    expr.ty = Some(crate::ty::Ty::Str);
}

/// `x_path(size: :small)` → `x_path(size: "small")`. A query value
/// reaches Rails' generator through `to_param`, and a Symbol's is its
/// spelling, so the literal is exact. The helper's parameter is typed
/// `String?` from the route's demand, and a strict target refuses the
/// Symbol at the call (`no implicit conversion of Symbol into String`
/// took two of campfire's logo tests). `format:` is not here: the
/// suffix pass ran first and consumed it.
fn ground_symbol_query_values(expr: &mut Expr, helpers: &std::collections::HashSet<String>) {
    let ExprNode::Send { recv, method, args, .. } = &mut *expr.node else { return };
    if !super::route_helper_receiver::is_helper_receiver(recv) || !helpers.contains(method.as_str()) {
        return;
    }
    let Some(last) = args.last_mut() else { return };
    let ExprNode::Hash { entries, kwargs: true } = &mut *last.node else { return };
    for (_, v) in entries.iter_mut() {
        if let ExprNode::Lit { value: Literal::Sym { value } } = &*v.node {
            let text = value.as_str().to_string();
            *v.node = ExprNode::Lit { value: Literal::Str { value: text } };
            v.ty = Some(crate::ty::Ty::Str);
        }
    }
}

fn strip_host_options(
    expr: &mut Expr,
    helpers: &std::collections::HashSet<String>,
) -> Option<(String, Expr, Option<Expr>)> {
    let ExprNode::Send { recv, method, args, block: None, .. } = &mut *expr.node else {
        return None;
    };
    if !super::route_helper_receiver::is_helper_receiver(recv) || !helpers.contains(method.as_str()) {
        return None;
    }
    let options = take_host_options(args);
    let stem = method.as_str().strip_suffix("_url")?;
    if options.only_path {
        *method = Symbol::from(format!("{stem}_path"));
        return None;
    }
    let host = options.host.or_else(|| recv.is_some().then(|| default_host(expr.span)))?;
    Some((stem.to_string(), host, options.protocol))
}

#[derive(Default)]
struct HostOptions {
    host: Option<Expr>,
    protocol: Option<Expr>,
    only_path: bool,
}

fn take_host_options(args: &mut Vec<Expr>) -> HostOptions {
    let mut options = HostOptions::default();
    let Some(ExprNode::Hash { entries, kwargs: true }) = args.last_mut().map(|arg| &mut *arg.node) else {
        return options;
    };
    for (k, v) in entries.iter() {
        let ExprNode::Lit { value: Literal::Sym { value } } = &*k.node else {
            continue;
        };
        match value.as_str() {
            "host" => options.host = Some(v.clone()),
            "protocol" => options.protocol = Some(v.clone()),
            "only_path" => {
                options.only_path = matches!(&*v.node, ExprNode::Lit { value: Literal::Bool { value: true } });
            }
            _ => {}
        }
    }
    entries.retain(|(k, _)| {
        !matches!(&*k.node, ExprNode::Lit { value: Literal::Sym { value } }
            if HOST_ONLY_OPTIONS.contains(&value.as_str()))
    });
    if entries.is_empty() {
        args.pop();
    }
    options
}

fn default_host(span: crate::span::Span) -> Expr {
    let rails = super::controller_to_library::rewrites::const_path(&["Rails"], span);
    let send = |recv, method| Expr::new(span, ExprNode::Send {
        recv: Some(recv), method: Symbol::from(method), args: vec![], block: None, parenthesized: false,
    });
    send(send(rails, "application"), "domain")
}
