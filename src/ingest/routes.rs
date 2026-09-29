//! `config/routes.rb` — parse the `Rails.application.routes.draw do … end`
//! DSL into a `RouteTable`. Recognizes verb shortcuts (`get`/`post`/…),
//! `root`, `resources`/`resource`, `namespace`/`scope`, and
//! `draw(:name)` inclusion of `config/routes/<name>.rb` split files.
//!
//! Recovery discipline: in survey mode an unsupported DSL construct
//! (`mount` of an external engine, `use_doorkeeper`, `devise_for`, …)
//! records a gap and drops that one entry — the rest of the table still
//! flattens. Source-local engine mounts are expanded by the app walker.
//! Not-modeled ≠ absent: a dropped entry is a ledger line, never a
//! silently empty route table.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use ruby_prism::Node;

use crate::dialect::{DirectHelper, HttpMethod, ResourceScope, RouteSpec, RouteTable};
use crate::naming::camelize;
use crate::{ClassId, Symbol};

use super::engines::LocalEngine;
use super::util::{
    constant_id_str, constant_path_segments_strs, find_call_named, flatten_statements,
    string_value, symbol_list_value, symbol_or_string_value, symbol_value,
};
use super::{IngestError, IngestResult};

/// Split route name mapped to its Ruby source and diagnostic path.
pub type DrawSources = HashMap<String, (Vec<u8>, String)>;

pub fn ingest_routes(source: &[u8], file: &str) -> IngestResult<RouteTable> {
    ingest_routes_with_draws(source, file, &HashMap::new())
}

/// `draws` maps a `draw(:name)` symbol to the split file Rails loads
/// into the same DSL context (`config/routes/<name>.rb`): name →
/// (source, path). The app ingester reads the directory; tests pass
/// maps directly.
pub fn ingest_routes_with_draws(
    source: &[u8],
    file: &str,
    draws: &DrawSources,
) -> IngestResult<RouteTable> {
    ingest_routes_with_local_engines(source, file, draws, &HashMap::new())
}

/// Parse an app's routes while expanding mounts of source-local engines.
/// Unknown/external engine mounts retain the existing survey behavior.
pub(super) fn ingest_routes_with_local_engines(
    source: &[u8],
    file: &str,
    draws: &DrawSources,
    engines: &HashMap<String, LocalEngine>,
) -> IngestResult<RouteTable> {
    let mut context = RouteContext {
        draws,
        engines,
        active_files: Vec::new(),
        direct_helpers: Vec::new(),
        redirects: Vec::new(),
        engine_depth: 0,
        optional_mount_prefix: false,
    };
    let entries = context.read(source, file, RouteSource::DrawBlock, None)?;
    Ok(RouteTable {
        entries,
        direct_helpers: context.direct_helpers,
        redirects: context.redirects,
    })
}

/// One route expansion owns all mutable state. Nested draws and mounts share
/// redirect names and cycle detection, but swap their own source lookup table.
struct RouteContext<'a> {
    draws: &'a DrawSources,
    engines: &'a HashMap<String, LocalEngine>,
    active_files: Vec<String>,
    direct_helpers: Vec<DirectHelper>,
    redirects: Vec<crate::dialect::RedirectRoute>,
    engine_depth: usize,
    optional_mount_prefix: bool,
}

#[derive(Clone, Copy)]
enum RouteSource {
    DrawBlock,
    Split,
}

impl RouteContext<'_> {
    fn read(
        &mut self,
        source: &[u8],
        file: &str,
        source_kind: RouteSource,
        parent: Option<&str>,
    ) -> IngestResult<Vec<RouteSpec>> {
        if self.active_files.iter().any(|active| active == file) {
            return Err(IngestError::Unsupported {
                file: file.into(),
                message: "recursive route draw or engine mount".into(),
            });
        }
        super::sources::register(file, &String::from_utf8_lossy(source));
        let parsed = super::prism::parse(source, file);
        let root = parsed.node();
        let body = match source_kind {
            RouteSource::DrawBlock => find_call_named(&root, "draw")
                .and_then(|call| call.block())
                .and_then(|block| block.as_block_node())
                .and_then(|block| block.body()),
            RouteSource::Split => root
                .as_program_node()
                .map(|program| program.statements().as_node()),
        };
        let Some(body) = body else {
            return Ok(Vec::new());
        };
        self.active_files.push(file.to_owned());
        let result = ingest_route_body(body, file, parent, self);
        self.active_files.pop();
        result
    }

    fn redirect(&mut self, path: &str, location: String, status: u16) -> Symbol {
        let base = redirect_action_name(path);
        let mut name = base.clone();
        let mut n = 1;
        while self.redirects.iter().any(|r| r.action.as_str() == name) {
            n += 1;
            name = format!("{base}_{n}");
        }
        let action = Symbol::from(name);
        self.redirects.push(crate::dialect::RedirectRoute {
            action: action.clone(),
            location,
            status,
        });
        action
    }
}

/// Mounted constant names in a routes file, used by the app walker to
/// match `mount X::Engine` against local engine roots before ingestion.
pub(super) fn mounted_engine_constants(
    source: &[u8],
    file: &str,
    draws: &DrawSources,
) -> Vec<String> {
    fn walk(
        node: Node<'_>,
        draws: &DrawSources,
        visited: &mut HashSet<String>,
        out: &mut Vec<String>,
    ) {
        if let Some(stmts) = node.as_statements_node() {
            for stmt in stmts.body().iter() {
                walk(stmt, draws, visited, out);
            }
            return;
        }
        let Some(call) = node.as_call_node() else {
            return;
        };
        match constant_id_str(&call.name()) {
            "mount" => {
                if let Some((constant, _)) = mount_target(&call) {
                    out.push(constant);
                }
            }
            "draw" if call.block().is_none() => {
                if let Some(name) = first_name_arg(&call)
                    && visited.insert(name.clone())
                    && let Some((source, file)) = draws.get(&name)
                {
                    let parsed = super::prism::parse(source, file);
                    if let Some(program) = parsed.node().as_program_node() {
                        walk(program.statements().as_node(), draws, visited, out);
                    }
                }
            }
            _ => {}
        }
        if let Some(body) = call
            .block()
            .and_then(|b| b.as_block_node())
            .and_then(|b| b.body())
        {
            walk(body, draws, visited, out);
        }
    }
    let parsed = super::prism::parse(source, file);
    let mut out = Vec::new();
    if let Some(program) = parsed.node().as_program_node() {
        walk(
            program.statements().as_node(),
            draws,
            &mut HashSet::new(),
            &mut out,
        );
    }
    out.sort();
    out.dedup();
    out
}

/// Both Rails mount spellings: `mount Blog::Engine, at: '/'` and
/// `mount Blog::Engine => '/'`. Discovery and expansion must agree.
fn mount_target(call: &ruby_prism::CallNode<'_>) -> Option<(String, Option<String>)> {
    for argument in call.arguments()?.arguments().iter() {
        if let Some(parts) = constant_path_segments_strs(&argument) {
            return Some((parts.join("::"), None));
        }
        if let Some(hash) = argument.as_keyword_hash_node() {
            for element in hash.elements().iter() {
                let Some(assoc) = element.as_assoc_node() else {
                    continue;
                };
                if let Some(parts) = constant_path_segments_strs(&assoc.key()) {
                    return Some((parts.join("::"), symbol_or_string_value(&assoc.value())));
                }
            }
        }
    }
    None
}

fn ingest_direct_helper(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
) -> IngestResult<Option<DirectHelper>> {
    let Some(name) = call
        .arguments()
        .and_then(|args| args.arguments().iter().next().and_then(|a| symbol_value(&a)))
    else {
        return Ok(None);
    };
    let Some(block) = call.block().and_then(|b| b.as_block_node()) else {
        return Ok(None);
    };
    let params: Vec<Symbol> = block
        .parameters()
        .and_then(|p| p.as_block_parameters_node())
        .and_then(|p| p.parameters())
        .map(|pn| {
            pn.requireds()
                .iter()
                .filter_map(|r| {
                    r.as_required_parameter_node()
                        .map(|rp| Symbol::from(constant_id_str(&rp.name())))
                })
                .collect()
        })
        .unwrap_or_default();
    let Some(body) = block.body() else {
        return Ok(None);
    };
    let body = super::expr::ingest_expr(&body, file)?;
    Ok(Some(DirectHelper { name: Symbol::from(name), params, body }))
}

/// Walk the statements inside a `routes.draw do ... end` block (or a
/// nested `resources :x do ... end` block) and collect their `RouteSpec`
/// entries. Recognized forms: verb shortcuts, `root "c#a"`,
/// `resources`/`resource`, `namespace`/`scope`, and `draw(:name)`.
/// `parent` carries the enclosing `resources :<name>` (its plural name)
/// so bare-verb member/nested shortcuts (`get "suggest"` with no `to:`)
/// can infer their controller; `None` at the top level.
fn ingest_route_body(
    body: Node<'_>,
    file: &str,
    parent: Option<&str>,
    context: &mut RouteContext<'_>,
) -> IngestResult<Vec<RouteSpec>> {
    ingest_route_stmts(flatten_statements(body).into_iter(), file, parent, context)
}

fn ingest_route_stmts<'pr>(
    stmts: impl Iterator<Item = Node<'pr>>,
    file: &str,
    parent: Option<&str>,
    context: &mut RouteContext<'_>,
) -> IngestResult<Vec<RouteSpec>> {
    let mut entries = Vec::new();
    for stmt in stmts {
        let Some(call) = stmt.as_call_node() else { continue };
        if call.receiver().is_some() {
            // `Rails.application.routes.draw` gets re-found as a nested
            // call when we walk a weird input; skip anything with an
            // explicit receiver here.
            continue;
        }
        let method = constant_id_str(&call.name()).to_string();

        // Block-wrapping DSLs we passthrough by flattening their
        // block contents into the outer entry list:
        //
        //   - `constraints :id => /regex/ do …` — restricts URL
        //     param matching; the route still resolves to the same
        //     controller#action.
        //   - `member do …` / `collection do …` (Rails resource-
        //     scoping wrappers) — these DO change the id segment the
        //     flattener prepends (`/resource/:id/reply` for member,
        //     `/resource/search` for collection, vs the bare-verb
        //     default `/resource/:resource_id/…`), so we tag each
        //     flattened child with its `ResourceScope` and let the
        //     flattener build the right path. `find_comment` reading
        //     `params[:id]` depends on the member routes carrying `:id`.
        if matches!(method.as_str(), "constraints" | "member" | "collection") {
            if let Some(block_node) = call.block() {
                if let Some(block) = block_node.as_block_node() {
                    if let Some(inner_body) = block.body() {
                        let mut inner =
                            ingest_route_body(inner_body, file, parent, context)?;
                        let scope = match method.as_str() {
                            "member" => Some(ResourceScope::Member),
                            "collection" => Some(ResourceScope::Collection),
                            _ => None, // constraints: no scope change
                        };
                        if let Some(scope) = scope {
                            apply_resource_scope(&mut inner, scope);
                        }
                        entries.extend(inner);
                    }
                }
            }
            continue;
        }

        // Per-entry recovery: one `mount`/`use_doorkeeper` must not
        // zero the whole table. Survey mode records the gap and keeps
        // walking; strict mode still fails loud.
        match ingest_route_call(&call, &method, file, parent, context) {
            Ok(Some(spec)) => entries.push(spec),
            Ok(None) => {}
            Err(err) if super::survey::is_active() => super::survey::record(&err),
            Err(err) => return Err(err),
        }
    }
    Ok(entries)
}

fn apply_resource_scope(entries: &mut [RouteSpec], scope: ResourceScope) {
    for entry in entries {
        match entry {
            RouteSpec::Explicit { scope: current, .. } => *current = scope,
            RouteSpec::Scope { entries, .. } => apply_resource_scope(entries, scope),
            _ => {}
        }
    }
}

fn redirect_action_name(path: &str) -> String {
    let mut name: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    name = name.trim_matches('_').to_string();
    while name.contains("__") {
        name = name.replace("__", "_");
    }
    if name.is_empty() {
        return "root".to_string();
    }
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        return format!("redirect_{name}");
    }
    name
}

/// The controller the synthesized redirect actions live on. Named for
/// what it is so an emitted tree reads honestly; an app that happens to
/// define this class would collide, which is why the name is one no
/// generator produces.
pub const REDIRECT_CONTROLLER: &str = "RoundhouseRedirectsController";

/// `redirect("/path")` / `redirect("/path", status: 302)` — the literal
/// form, which is all that can be served without running Rails'
/// redirect block. Answers the location and the status Rails would use.
fn redirect_literal(node: &Node<'_>) -> Option<(String, u16)> {
    let call = node.as_call_node()?;
    if call.receiver().is_some() {
        return None;
    }
    let name = call.name();
    if constant_id_str(&name) != "redirect" {
        return None;
    }
    // A block form (`redirect { |params, req| … }`) has no literal to
    // carry and stays dropped.
    if call.block().is_some() {
        return None;
    }
    let arguments = call.arguments()?;
    let mut location = None;
    let mut status = 301;
    for argument in arguments.arguments().iter() {
        if let Some(s) = string_value(&argument) {
            location.get_or_insert(s);
            continue;
        }
        let Some(hash) = argument.as_keyword_hash_node() else { return None };
        for element in hash.elements().iter() {
            let Some(assoc) = element.as_assoc_node() else { continue };
            let Some(key) = symbol_value(&assoc.key()) else { continue };
            if key.as_str() != "status" {
                return None;
            }
            let value = assoc.value();
            let code = value
                .as_integer_node()
                .and_then(|i| super::util::integer_i64(&i.value()))
                .and_then(|i| u16::try_from(i).ok())?;
            status = code;
        }
    }
    Some((location?, status))
}

fn ingest_route_call(
    call: &ruby_prism::CallNode<'_>,
    method: &str,
    file: &str,
    parent: Option<&str>,
    context: &mut RouteContext<'_>,
) -> IngestResult<Option<RouteSpec>> {
    if http_method_from(method).is_none() && !matches!(method, "root" | "resources" | "resource") {
        return ingest_route_spec(call, method, file, parent, context);
    }

    let mut defaults = IndexMap::new();
    if let Some(args) = call.arguments() {
        for arg in args.arguments().iter() {
            let Some(hash) = arg.as_keyword_hash_node() else { continue };
            for element in hash.elements().iter() {
                let Some(assoc) = element.as_assoc_node() else { continue };
                if symbol_value(&assoc.key()).as_deref() == Some("defaults") {
                    defaults = ingest_route_defaults(&assoc.value(), file)?;
                }
            }
        }
    }

    let route = ingest_route_spec(call, method, file, parent, context)?;
    Ok(route.map(|route| {
        if defaults.is_empty() {
            route
        } else {
            RouteSpec::Scope {
                path: None,
                module: None,
                as_prefix: None,
                defaults,
                nest: false,
                entries: vec![route],
            }
        }
    }))
}

fn ingest_route_spec(
    call: &ruby_prism::CallNode<'_>,
    method: &str,
    file: &str,
    parent: Option<&str>,
    context: &mut RouteContext<'_>,
) -> IngestResult<Option<RouteSpec>> {
    // Verb shortcuts (`get "/p", to: "c#a"` and the hashrocket form
    // `get "/p" => "c#a"`). `ingest_explicit_route` returns Ok(None)
    // for shapes it intentionally drops (today: `to: redirect(...)`
    // helpers — not bench-critical, not modeled in `RouteSpec`).
    if let Some(http) = http_method_from(method) {
        return ingest_explicit_route(call, http, file, parent, context);
    }
    match method {
        "root" => ingest_root_route(call, file, context),
        "resources" => ingest_resources_route(call, file, context, false).map(Some),
        "resource" => ingest_resources_route(call, file, context, true).map(Some),
        "namespace" => ingest_namespace_route(call, file, context).map(Some),
        "scope" => ingest_scope_route(call, file, context).map(Some),
        "defaults" => ingest_defaults_route(call, file, parent, context).map(Some),
        // `nested do … end` — the explicit form of the nesting a
        // `resources` block already applies to a child `resources` or
        // verb call. It carries no facets of its own; what it does is
        // force the ENCLOSING resource's `/parent/:parent_id` prefix to
        // be materialized before anything inside runs, which is the
        // only way a `scope path:` lands inside it rather than in front
        // of it (`scope` is not one of the calls Rails auto-nests).
        "nested" => Ok(Some(RouteSpec::Scope {
            path: None,
            module: None,
            as_prefix: None,
            defaults: IndexMap::new(),
            nest: true,
            entries: block_entries(call, file, None, None, context)?,
        })),
        "draw" => ingest_draw_route(call, file, parent, context),
        // `mount SomeEngine, at: "/path"` — the app-level ingester
        // expands a source-local engine; a gem-provided engine (such
        // as mission_control or sidekiq-web) remains an explicit gap.
        "mount" => ingest_mount_route(call, file, context),
        // Custom helpers have bodies but no dispatch path. Collect them in
        // the same walk so split route files participate too.
        "direct" => {
            if context.engine_depth > 0 {
                return Err(IngestError::Unsupported {
                    file: file.into(), message: "direct helpers inside local engines are not yet supported".into(),
                });
            }
            if let Some(helper) = ingest_direct_helper(call, file)? {
                context.direct_helpers.push(helper);
            }
            Ok(None)
        },
        // Unknown DSL — `concern`, `devise_for`,
        // `use_doorkeeper`, `authenticate`, etc. land here. Strict
        // ingest fails loud so the fixture that introduces them forces
        // a recognizer; survey callers get a per-entry ledger line
        // (see ingest_route_stmts).
        _ => Err(IngestError::Unsupported {
            file: file.into(),
            message: format!("unsupported routes DSL: `{method}`"),
        }),
    }
}

/// Expand a mount only when the app-level walker found matching local
/// engine source. A gem mount remains a visible survey gap, as before.
fn ingest_mount_route(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
    context: &mut RouteContext<'_>,
) -> IngestResult<Option<RouteSpec>> {
    let Some(args) = call.arguments() else {
        return record_external_mount(file);
    };
    let Some((engine_name, hash_path)) = mount_target(call) else {
        return record_external_mount(file);
    };
    let Some(engine) = context.engines.get(&engine_name) else {
        return record_external_mount(file);
    };

    let unsupported = || IngestError::Unsupported {
        file: file.into(),
        message: format!(
            "local engine mount `{engine_name}` supports literal `at:` and `as:` options only"
        ),
    };
    let mut at = hash_path;
    let mut helper_prefix = engine.helper_prefix.clone();
    for argument in args.arguments().iter() {
        let Some(hash) = argument.as_keyword_hash_node() else {
            continue;
        };
        for element in hash.elements().iter() {
            let assoc = element.as_assoc_node().ok_or_else(unsupported)?;
            if constant_path_segments_strs(&assoc.key()).is_some() {
                continue;
            }
            let key = symbol_value(&assoc.key()).ok_or_else(unsupported)?;
            let value = symbol_or_string_value(&assoc.value()).ok_or_else(unsupported)?;
            match key.as_str() {
                "at" => at = Some(value),
                "as" => helper_prefix = value,
                _ => return Err(unsupported()),
            }
        }
    }
    let at = at.ok_or_else(unsupported)?;
    if context.optional_mount_prefix || has_optional_segments(&at) {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: format!(
                "optional path segments in local engine mount `{engine_name}` are not yet supported"
            ),
        });
    }
    let previous_draws = std::mem::replace(&mut context.draws, &engine.draws);
    context.engine_depth += 1;
    let entries = context.read(&engine.source, &engine.file, RouteSource::DrawBlock, None);
    context.draws = previous_draws;
    context.engine_depth -= 1;
    let entries = entries?;
    Ok(Some(RouteSpec::Mount {
        path: at,
        module: engine.module.clone(),
        as_prefix: helper_prefix,
        source_root: engine.root.display().to_string(),
        entries,
    }))
}

fn record_external_mount(file: &str) -> IngestResult<Option<RouteSpec>> {
    if super::survey::is_active() {
        super::survey::record(&IngestError::Unsupported {
            file: file.into(),
            message: "route dropped: `mount` of an external engine".into(),
        });
    }
    Ok(None)
}

fn http_method_from(name: &str) -> Option<HttpMethod> {
    Some(match name {
        "get" => HttpMethod::Get,
        "post" => HttpMethod::Post,
        "put" => HttpMethod::Put,
        "patch" => HttpMethod::Patch,
        "delete" => HttpMethod::Delete,
        "head" => HttpMethod::Head,
        "options" => HttpMethod::Options,
        "match" => HttpMethod::Any,
        _ => return None,
    })
}

/// First positional symbol-or-string argument (`namespace :admin`,
/// `scope "v2"`, `draw(:api)`).
fn first_name_arg(call: &ruby_prism::CallNode<'_>) -> Option<String> {
    let args = call.arguments()?;
    for arg in args.arguments().iter() {
        if let Some(s) = symbol_value(&arg) {
            return Some(s);
        }
        if let Some(s) = string_value(&arg) {
            return Some(s);
        }
        // Keyword hash → options-only call (`scope module: :web`).
        if arg.as_keyword_hash_node().is_some() {
            return None;
        }
    }
    None
}

fn has_optional_segments(path: &str) -> bool {
    path.contains('(') || path.contains(')')
}

fn block_entries(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
    parent: Option<&str>,
    path: Option<&str>,
    context: &mut RouteContext<'_>,
) -> IngestResult<Vec<RouteSpec>> {
    let previous = context.optional_mount_prefix;
    context.optional_mount_prefix |= path.is_some_and(has_optional_segments);
    let entries = match call.block() {
        Some(block_node) => match block_node.as_block_node() {
            Some(block) => match block.body() {
                Some(body) => ingest_route_body(body, file, parent, context),
                None => Ok(Vec::new()),
            },
            None => Ok(Vec::new()),
        },
        None => Ok(Vec::new()),
    };
    context.optional_mount_prefix = previous;
    entries
}

/// `namespace :admin do … end` — `scope` with path, controller module,
/// and helper prefix all set to the name. Resets the enclosing
/// `resources` inference context (Rails does not infer member
/// controllers across a namespace boundary).
fn ingest_namespace_route(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
    context: &mut RouteContext<'_>,
) -> IngestResult<RouteSpec> {
    let Some(name) = first_name_arg(call) else {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: "namespace without a name".into(),
        });
    };
    let entries = block_entries(call, file, None, Some(&name), context)?;
    Ok(RouteSpec::Scope {
        path: Some(name.clone()),
        module: Some(name.clone()),
        as_prefix: Some(name),
        defaults: IndexMap::new(),
        nest: false,
        entries,
    })
}

/// `scope <path> [, path:, module:, as:] do … end` — each facet
/// independent. A positional symbol/string is the path segment
/// (`scope :v1_alpha, as: :v1_alpha, module: :v1`).
fn ingest_scope_route(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
    context: &mut RouteContext<'_>,
) -> IngestResult<RouteSpec> {
    let mut path = first_name_arg(call);
    let mut module: Option<String> = None;
    let mut as_prefix: Option<String> = None;
    let mut defaults: IndexMap<Symbol, String> = IndexMap::new();
    if let Some(args) = call.arguments() {
        for arg in args.arguments().iter() {
            let Some(kh) = arg.as_keyword_hash_node() else { continue };
            for el in kh.elements().iter() {
                let Some(assoc) = el.as_assoc_node() else { continue };
                let Some(key) = symbol_value(&assoc.key()) else { continue };
                let value = assoc.value();
                let val = symbol_value(&value).or_else(|| string_value(&value));
                match key.as_str() {
                    "path" => path = val.or(path),
                    "module" => module = val,
                    "as" => as_prefix = val,
                    // `defaults: { user_id: "me" }` fills a dynamic
                    // segment the caller omits, which makes the
                    // generated helper's parameter OPTIONAL — this used
                    // to be dropped as "shapes the request, not the
                    // (path, controller, action) triple", and that is
                    // true of the triple but not of the signature.
                    // campfire calls `user_profile_url` with no argument.
                    "defaults" => {
                        defaults = ingest_route_defaults(&value, file)?;
                    }
                    // `constraints:` / `format:` shape the request, not
                    // the (path, controller, action) triple.
                    _ => {}
                }
            }
        }
    }
    let entries = block_entries(call, file, None, path.as_deref(), context)?;
    Ok(RouteSpec::Scope { path, module, as_prefix, defaults, nest: false, entries })
}

fn ingest_route_defaults(node: &Node<'_>, file: &str) -> IngestResult<IndexMap<Symbol, String>> {
    let elements = node
        .as_hash_node()
        .map(|hash| hash.elements())
        .or_else(|| node.as_keyword_hash_node().map(|hash| hash.elements()))
        .ok_or_else(|| IngestError::Unsupported {
            file: file.into(),
            message: "route defaults must be a literal hash".into(),
        })?;
    let mut defaults = IndexMap::new();
    for element in elements.iter() {
        let pair = element.as_assoc_node().and_then(|assoc| {
            Some((symbol_value(&assoc.key())?, symbol_or_string_value(&assoc.value())?))
        });
        let Some((key, value)) = pair else {
            return Err(IngestError::Unsupported {
                file: file.into(),
                message: "route defaults require literal symbol keys and symbol or string values; \
                          string keys are not modeled"
                    .into(),
            });
        };
        defaults.insert(Symbol::from(key), value);
    }
    Ok(defaults)
}

fn ingest_defaults_route(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
    parent: Option<&str>,
    context: &mut RouteContext<'_>,
) -> IngestResult<RouteSpec> {
    let mut defaults = IndexMap::new();
    if let Some(args) = call.arguments() {
        for arg in args.arguments().iter() {
            defaults.extend(ingest_route_defaults(&arg, file)?);
        }
    }
    Ok(RouteSpec::Scope {
        path: None,
        module: None,
        as_prefix: None,
        defaults,
        nest: false,
        entries: block_entries(call, file, parent, None, context)?,
    })
}

/// `draw(:admin)` — Rails loads `config/routes/admin.rb` into the same
/// DSL context. The split file's top-level statements are route DSL
/// directly (no `routes.draw` wrapper). Included entries ride a
/// facet-less Scope so the flattener composes them transparently.
fn ingest_draw_route(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
    parent: Option<&str>,
    context: &mut RouteContext<'_>,
) -> IngestResult<Option<RouteSpec>> {
    let Some(name) = first_name_arg(call) else {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: "draw without a route-file name".into(),
        });
    };
    let Some((source, path)) = context.draws.get(&name) else {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: format!("draw(:{name}) — config/routes/{name}.rb not found"),
        });
    };
    let entries = context.read(source, path, RouteSource::Split, parent)?;
    Ok(Some(RouteSpec::Scope {
        path: None,
        module: None,
        as_prefix: None,
        defaults: IndexMap::new(),
        nest: false,
        entries,
    }))
}

/// Raw regex-pattern source of a `/.../ ` literal value node — the text
/// between the delimiters, verbatim (escapes like `\/` preserved so it
/// drops straight back into a Ruby regex literal), None for non-regex
/// values. Used for `constraints:` param restrictions (#67).
fn regex_source(node: &Node<'_>) -> Option<String> {
    let r = node.as_regular_expression_node()?;
    Some(String::from_utf8_lossy(r.content_loc().as_slice()).into_owned())
}

fn ingest_explicit_route(
    call: &ruby_prism::CallNode<'_>,
    method: HttpMethod,
    file: &str,
    parent: Option<&str>,
    context: &mut RouteContext<'_>,
) -> IngestResult<Option<RouteSpec>> {
    let Some(args_node) = call.arguments() else {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: "verb route without arguments".into(),
        });
    };
    let mut path: Option<String> = None;
    let mut to: Option<String> = None;
    let mut to_is_unsupported = false;
    let mut redirect_target: Option<(String, u16)> = None;
    let mut as_name: Option<Symbol> = None;
    let mut action_kwarg: Option<String> = None;
    // The INLINE spelling of `member do … end` / `collection do … end`.
    // Rails accepts both, and campfire writes `delete :clear, on:
    // :collection`; only the block form was recognized, so the route was
    // nested as `/searches/:search_id/clear` (named `search_clear`)
    // where Rails serves `/searches/clear` (named `clear_searches`).
    let mut on_scope: Option<ResourceScope> = None;
    let mut constraints: IndexMap<Symbol, String> = IndexMap::new();

    for arg in args_node.arguments().iter() {
        if let Some(s) = string_value(&arg) {
            // Positional string arg — the path: `get "/p", to: "c#a"`.
            if path.is_none() {
                path = Some(s);
            }
        } else if arg.as_symbol_node().is_some() && path.is_none() && to.is_none() {
            // Positional SYMBOL arg — the same shortcut in the other
            // spelling: `member do get :doff end` is identical to
            // `get "doff"` in Rails, and lobsters' hats routes use it
            // exclusively (`get :doff`, `post :doff_by_user`,
            // `post :update_in_place`, `post :update_by_recreating`).
            // Accepting only the String form silently dropped those four
            // routes and their helpers. Guarded on `to.is_none()` so a
            // symbol appearing after a target can't be mistaken for one.
            if let Some(s) = symbol_value(&arg) {
                path = Some(s);
            }
        } else if let Some(kh) = arg.as_keyword_hash_node() {
            // Two shapes share KeywordHashNode here:
            //   1. Modern kwargs hash: `get "/p", to: "c#a", as: :n` —
            //      path is the prior positional, this hash is all kwargs.
            //   2. Hashrocket-style routing: `get "/p" => "c#a", :as => :n`
            //      — the FIRST entry's key is a String (the path) and
            //      its value is the target string. Subsequent entries
            //      are kwargs (Symbol-keyed).
            for el in kh.elements().iter() {
                let Some(assoc) = el.as_assoc_node() else { continue };
                let key_node = assoc.key();
                let value = &assoc.value();

                // String-keyed entry → path → target pair (hashrocket
                // form). Only consume the first such entry as path.
                if let Some(key_str) = string_value(&key_node) {
                    if path.is_none() {
                        path = Some(key_str);
                        if let Some(v) = string_value(value) {
                            to = Some(v);
                        } else {
                            // `get "/p" => redirect("/q")` — the
                            // hashrocket spelling of the same literal
                            // redirect the kwarg form takes below.
                            match redirect_literal(value) {
                                Some(r) => redirect_target = Some(r),
                                None => to_is_unsupported = true,
                            }
                        }
                        continue;
                    }
                }

                // Symbol-keyed entry → standard kwarg.
                let Some(key_sym) = symbol_value(&key_node) else { continue };
                match key_sym.as_str() {
                    "to" => {
                        if let Some(v) = string_value(value) {
                            to = Some(v);
                        } else if let Some(r) = redirect_literal(value) {
                            redirect_target = Some(r);
                        } else {
                            // A block redirect (`redirect { |p, req| … }`)
                            // carries no literal to serve, so it stays
                            // dropped — with the ledger line #82 added.
                            to_is_unsupported = true;
                        }
                    }
                    // `:as` accepts either a symbol (`as: :user`) or a
                    // string (`:as => "user"`); lobsters uses the string
                    // form throughout. Without the string fallback the name
                    // was dropped and the helper fell back to the action
                    // name (`show_path` for `user_path`), leaving every
                    // `user_path`/`tag_path`/… call unresolved.
                    "as" => {
                        as_name = symbol_value(value)
                            .map(Symbol::from)
                            .or_else(|| string_value(value).map(Symbol::from));
                    }
                    "on" => {
                        on_scope = match symbol_value(value).as_deref() {
                            Some("member") => Some(ResourceScope::Member),
                            Some("collection") => Some(ResourceScope::Collection),
                            _ => None,
                        };
                    }
                    // `post "suggest", :action => "submit_suggestions"` —
                    // the action override for a resource-scoped shortcut.
                    "action" => {
                        action_kwarg =
                            string_value(value).or_else(|| symbol_value(value));
                    }
                    // `via: :all` (HTTP-method override) and similar
                    // method-shaping options aren't modeled today; the
                    // route still resolves to the outer verb. Other
                    // string-value options become routing constraints.
                    "via" => {}
                    // `constraints: { id: /\d+/, tag: /[^,.\/]+/ }` —
                    // per-param regex restrictions. Capture each param's
                    // regex SOURCE (raw text between the delimiters, so
                    // `\/` etc. survive back into a Ruby literal) keyed
                    // by param name. digit-class regexes drive the runtime
                    // router's Integer matcher; the rest let the roda
                    // converter disambiguate two routes that share a
                    // path+verb and differ only by the constraint (#67).
                    "constraints" => {
                        if let Some(h) = value.as_hash_node() {
                            for el in h.elements().iter() {
                                let Some(a) = el.as_assoc_node() else { continue };
                                let Some(param) = symbol_value(&a.key()) else { continue };
                                if let Some(src) = regex_source(&a.value()) {
                                    constraints.insert(Symbol::from(param.as_str()), src);
                                }
                            }
                        }
                    }
                    other => {
                        if let Some(v) = string_value(value) {
                            constraints.insert(Symbol::from(other), v);
                        }
                    }
                }
            }
        }
    }

    if let Some((location, status)) = redirect_target {
        // Served by a synthesized action rather than dropped: the app
        // gets the 301 it asked for, and no emitter learns a new route
        // kind for it.
        let path = path.clone().unwrap_or_else(|| "/".to_string());
        let action = context.redirect(&path, location, status);
        return Ok(Some(RouteSpec::Explicit {
            method,
            path,
            controller: ClassId(Symbol::from(REDIRECT_CONTROLLER)),
            action,
            as_name,
            constraints: IndexMap::new(),
            scope: ResourceScope::default(),
        }));
    }
    if to_is_unsupported {
        // Dropped, with a ledger line: the route is not modeled
        // (`RouteSpec` has no Redirect variant), and a drop nobody can
        // see is how #82's `root to: redirect(...)` went unnoticed.
        // Strict runs still pass — the hole is a missing route, not a
        // miscompile — so this records rather than errors.
        super::survey::record(&IngestError::Unsupported {
            file: file.into(),
            message: format!(
                "route dropped: `{}` with a non-string target (`to: redirect(...)` is not modeled)",
                path.as_deref().unwrap_or("?")
            ),
        });
        return Ok(None);
    }

    let (controller, action) = match to.as_deref().and_then(|s| s.split_once('#')) {
        Some((c, a)) => (c.to_string(), a.to_string()),
        None => {
            // No `to:` and no hashrocket target — a resource-scoped
            // shortcut (`get "suggest"` / `post "suggest", :action =>
            // "submit_suggestions"` inside `resources :stories do`).
            // Controller comes from the enclosing resources block; the
            // action is the `:action` kwarg, else the path stem. The
            // flattener nests the path under `/:<parent>_id` and names
            // the helper `<singular>_<stem>` (`story_suggest_path`).
            // Outside a resources block there's nothing to infer from
            // (a rare typo shape) — keep the silent drop.
            let Some(parent) = parent else {
                return Ok(None);
            };
            let Some(p) = path.as_deref() else {
                return Ok(None);
            };
            let stem = p.trim_matches('/').to_string();
            if stem.is_empty() || stem.contains('/') || stem.contains(':') {
                return Ok(None);
            }
            path = Some(format!("/{stem}"));
            (parent.to_string(), action_kwarg.unwrap_or(stem))
        }
    };

    Ok(Some(RouteSpec::Explicit {
        method,
        path: path.unwrap_or_default(),
        controller: ClassId(Symbol::from(controller_class_name(&controller))),
        action: Symbol::from(action),
        as_name,
        constraints,
        // The inline `on:` kwarg wins; otherwise Nested is the default
        // and a `member do`/`collection do` wrapper (handled in
        // `ingest_route_body`) overwrites it on the returned entry.
        scope: on_scope.unwrap_or(ResourceScope::Nested),
    }))
}

fn ingest_root_route(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
    context: &mut RouteContext<'_>,
) -> IngestResult<Option<RouteSpec>> {
    // Two forms:
    //   1. `root "c#a"` — single positional string arg.
    //   2. `root to: "c#a", as: "root"` — kwargs hash (modern or
    //      hashrocket `:to =>` style; both produce KeywordHashNode).
    // Any non-string `to:` (`root to: redirect("/scan")`, a lambda) is
    // the same drop as an explicit verb's `to: redirect(...)`: the
    // route is not modeled, so it is skipped with a ledger line. It
    // used to come back as a Root with an EMPTY target, which the
    // flattener turned into `Route.new("GET", "/", :, :index)` — the
    // one file in the tree that failed `ruby -c`, and the entry point
    // (#82).
    let mut target: Option<String> = None;
    let mut redirect_target: Option<(String, u16)> = None;
    if let Some(args_node) = call.arguments() {
        for arg in args_node.arguments().iter() {
            if let Some(s) = string_value(&arg) {
                if target.is_none() {
                    target = Some(s);
                }
            } else if let Some(kh) = arg.as_keyword_hash_node() {
                for el in kh.elements().iter() {
                    let Some(assoc) = el.as_assoc_node() else { continue };
                    let Some(key_sym) = symbol_value(&assoc.key()) else { continue };
                    if key_sym.as_str() == "to" {
                        if let Some(v) = string_value(&assoc.value()) {
                            target = Some(v);
                        } else if let Some(r) = redirect_literal(&assoc.value()) {
                            redirect_target = Some(r);
                        }
                    }
                }
            }
        }
    }
    if let Some(redirect) = redirect_target {
        // `root to: redirect("/scan")` — served by a synthesized action
        // rather than dropped, so the emitted app answers `/` the way
        // Rails does (#82 recorded the drop; this lowers it).
        let (location, status) = redirect;
        let action = context.redirect("/", location, status);
        return Ok(Some(RouteSpec::Explicit {
            method: HttpMethod::Get,
            path: "/".to_string(),
            controller: ClassId(Symbol::from(REDIRECT_CONTROLLER)),
            action,
            as_name: Some(Symbol::from("root")),
            constraints: IndexMap::new(),
            scope: ResourceScope::default(),
        }));
    }
    match target {
        Some(target) if !target.is_empty() => Ok(Some(RouteSpec::Root { target })),
        // Same contract as `mount` and the explicit verbs' redirect
        // drop: not an error, but never silent.
        _ => {
            super::survey::record(&IngestError::Unsupported {
                file: file.into(),
                message: "route dropped: `root` with a non-string target \
                          (`to: redirect(...)` is not modeled)"
                    .into(),
            });
            Ok(None)
        }
    }
}

fn ingest_resources_route(
    call: &ruby_prism::CallNode<'_>,
    file: &str,
    context: &mut RouteContext<'_>,
    singular: bool,
) -> IngestResult<RouteSpec> {
    let Some(args_node) = call.arguments() else {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: "resources call without a name".into(),
        });
    };
    let all_args = args_node.arguments();
    let mut iter = all_args.iter();
    let first = iter.next().ok_or_else(|| IngestError::Unsupported {
        file: file.into(),
        message: "resources call without a name".into(),
    })?;
    // `resources "tours"` is `resources :tours` — Rails `to_sym`s the
    // name (#85).
    let name_str = symbol_or_string_value(&first).ok_or_else(|| IngestError::Unsupported {
        file: file.into(),
        message: "resources name must be a symbol or string".into(),
    })?;
    let name = Symbol::from(name_str.as_str());

    let mut only: Vec<Symbol> = Vec::new();
    let mut except: Vec<Symbol> = Vec::new();
    let mut as_name: Option<Symbol> = None;
    let mut controller: Option<String> = None;
    let mut param: Option<Symbol> = None;
    for arg in iter {
        let Some(kh) = arg.as_keyword_hash_node() else { continue };
        for el in kh.elements().iter() {
            let Some(assoc) = el.as_assoc_node() else { continue };
            let Some(key) = symbol_value(&assoc.key()) else { continue };
            let value = assoc.value();
            match key.as_str() {
                // An `only:`/`except:` that is written but does not
                // parse to a literal list (a constant, a method call)
                // must NOT come back empty: the expander reads an
                // empty `only` as "all seven actions", which is the
                // opposite of what a restriction means (#85).
                "only" | "except" => {
                    let list = symbol_list_value(&value);
                    if list.is_empty() {
                        return Err(IngestError::Unsupported {
                            file: file.into(),
                            message: format!(
                                "resources :{name_str} `{key}:` is not a literal list of actions"
                            ),
                        });
                    }
                    if key.as_str() == "only" {
                        only = list;
                    } else {
                        except = list;
                    }
                }
                // `as:` renames the HELPERS, not the path — lobsters'
                // `namespace :mod { resources :mails, as: "mod_mails" }`
                // is `/mod/mails` served by `mod_mod_mails_path`. Dropping
                // it named those helpers `mod_mails_path`, which both
                // missed every call site and collided with the top-level
                // `resources :mod_mails`.
                "as" => as_name = symbol_or_string_value(&value).map(|s| Symbol::from(s.as_str())),
                // `controller:` moves the CLASS and nothing else — the
                // path still comes from the resource name and so do the
                // helpers. campfire's bot API is `resources :messages,
                // controller: "messages/by_bots"`, and dropping this
                // pointed five routes at `MessagesController`, which
                // answers them with the human HTML flow.
                "controller" => controller = symbol_or_string_value(&value),
                // `param: :task_id` renames the MEMBER SEGMENT: the
                // path binds `:task_id` and the controller reads
                // `params[:task_id]`. Dropped, the path bound `:id`
                // and the lowered action read nil (#84).
                "param" => param = symbol_or_string_value(&value).map(|s| Symbol::from(s.as_str())),
                // `path:` and `shallow:` land when a fixture demands them.
                _ => {}
            }
        }
    }

    let nested = block_entries(call, file, Some(&name_str), Some(&name_str), context)?;

    Ok(RouteSpec::Resources {
        name,
        only,
        except,
        nested,
        singular,
        as_name,
        controller,
        param,
    })
}

/// `"c"` / `"admin/c"` → `CController` / `Admin::CController`.
fn controller_class_name(short: &str) -> String {
    let mut s = short
        .split('/')
        .map(camelize)
        .collect::<Vec<_>>()
        .join("::");
    s.push_str("Controller");
    s
}
