//! Resolve engine-local and proxy route helpers before type analysis. Generated
//! helpers share one namespace, while Rails source uses `articles_path` inside
//! an engine and `blog.articles_path` from the host.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::routes::MountedHelpers;
use crate::App;
use crate::Symbol;
use crate::expr::{Expr, ExprNode};

pub(crate) fn normalize(app: &mut App) {
    let (_, mounts) = super::routes::flatten_with_mounts(app);
    if mounts.is_empty() {
        return;
    }
    let owners: Vec<_> = app
        .sources
        .iter()
        .map(|source| {
            unique_mount(&mounts, |mount| {
                Path::new(&source.path).starts_with(&mount.source_root)
            })
        })
        .collect();
    let shadows = owner_shadows(app);
    let empty = HashSet::new();
    let mut rewrite = |class: Option<&crate::ClassId>, body: &mut Expr| {
        let owner = class.and_then(|class| namespace_owner(&mounts, class.0.as_str(), "::"));
        let shadowed = class.and_then(|class| shadows.get(class)).unwrap_or(&empty);
        resolve(body, &mounts, &owners, shadowed, owner);
    };
    super::for_each_hook_body_with_owner(app, &mut rewrite);
    super::for_each_test_body_with_owner(app, &mut rewrite);
    for helper in &mut app.routes.direct_helpers {
        rewrite(None, &mut helper.body);
    }
    let view_shadows = super::route_helper_receiver::helper_methods_by_scope(app);
    let view_scopes: Vec<_> = app
        .views
        .iter()
        .map(|view| app.helper_scope_for(view.name.as_str(), view.body.span))
        .collect();
    for (view, scope) in app.views.iter_mut().zip(view_scopes) {
        let owner = namespace_owner(&mounts, view.name.as_str(), "/");
        let shadowed = view_shadows.get(&scope).unwrap_or(&empty);
        resolve(&mut view.body, &mounts, &owners, shadowed, owner);
    }
}

fn namespace_owner<'a>(
    mounts: &'a [MountedHelpers],
    name: &str,
    separator: &str,
) -> Option<&'a MountedHelpers> {
    let namespace = mounts
        .iter()
        .filter_map(|mount| mount.namespace.as_deref())
        .filter(|namespace| {
            let namespace = if separator == "/" {
                crate::naming::underscore(namespace)
            } else {
                namespace.to_string()
            };
            name.starts_with(&format!("{namespace}{separator}"))
        })
        .max_by_key(|namespace| namespace.len())?;
    unique_mount(mounts, |mount| {
        mount.namespace.as_deref() == Some(namespace)
    })
}

fn owner_shadows(app: &App) -> HashMap<crate::ClassId, HashSet<Symbol>> {
    #[derive(Default)]
    struct Methods {
        names: HashSet<Symbol>,
        ancestors: Vec<crate::ClassId>,
    }
    let mut methods: HashMap<crate::ClassId, Methods> = HashMap::new();
    for model in &app.models {
        let entry = methods.entry(model.name.clone()).or_default();
        entry.names.extend(model.methods().map(|m| m.name.clone()));
        entry.ancestors.extend(model.parent.iter().cloned());
        for item in &model.body {
            if let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item {
                entry.ancestors.extend(included_modules(expr));
            }
        }
    }
    for class in app
        .library_classes
        .iter()
        .chain(app.rails_application.iter())
    {
        let entry = methods.entry(class.name.clone()).or_default();
        entry
            .names
            .extend(class.methods.iter().map(|m| m.name.clone()));
        entry
            .ancestors
            .extend(class.parent.iter().chain(&class.includes).cloned());
    }
    for controller in &app.controllers {
        let entry = methods.entry(controller.name.clone()).or_default();
        entry
            .names
            .extend(controller.actions().map(|a| a.name.clone()));
        entry.ancestors.extend(controller.parent.iter().cloned());
        for item in &controller.body {
            if let crate::dialect::ControllerBodyItem::Unknown { expr, .. } = item {
                entry.ancestors.extend(included_modules(expr));
            }
        }
    }
    for test in &app.test_modules {
        let entry = methods.entry(test.name.clone()).or_default();
        entry
            .names
            .extend(test.helpers.iter().map(|m| m.name.clone()));
        entry
            .ancestors
            .extend(test.parent.iter().chain(&test.includes).cloned());
    }
    methods
        .keys()
        .map(|class| {
            let mut names = HashSet::new();
            let mut visited = HashSet::new();
            let mut pending = vec![class];
            while let Some(class) = pending.pop() {
                if !visited.insert(class) {
                    continue;
                }
                if let Some(entry) = methods.get(class) {
                    names.extend(entry.names.iter().cloned());
                    pending.extend(&entry.ancestors);
                }
            }
            (class.clone(), names)
        })
        .collect()
}

fn included_modules(expr: &Expr) -> Vec<crate::ClassId> {
    let ExprNode::Send {
        recv: None,
        method,
        args,
        ..
    } = &*expr.node
    else {
        return Vec::new();
    };
    if !matches!(method.as_str(), "include" | "prepend") {
        return Vec::new();
    }
    args.iter()
        .filter_map(|arg| {
            let ExprNode::Const { path } = &*arg.node else {
                return None;
            };
            Some(crate::ClassId(Symbol::from(
                path.iter()
                    .map(Symbol::as_str)
                    .collect::<Vec<_>>()
                    .join("::"),
            )))
        })
        .collect()
}

/// Ambiguous mounts require runtime context; leave them unresolved.
fn unique_mount(
    mounts: &[MountedHelpers],
    matches: impl Fn(&MountedHelpers) -> bool,
) -> Option<&MountedHelpers> {
    let mut matching = mounts.iter().filter(|mount| matches(mount));
    let mount = matching.next()?;
    matching.next().is_none().then_some(mount)
}

fn resolve(
    expr: &mut Expr,
    mounts: &[MountedHelpers],
    owners: &[Option<&MountedHelpers>],
    shadows: &HashSet<Symbol>,
    context_owner: Option<&MountedHelpers>,
) {
    expr.node
        .for_each_child_mut(&mut |child| resolve(child, mounts, owners, shadows, context_owner));
    let source_owner = expr
        .span
        .file
        .0
        .checked_sub(1)
        .and_then(|i| owners.get(i as usize))
        .copied()
        .flatten();
    let owner = context_owner.or(source_owner);
    let ExprNode::Send { recv, method, .. } = &mut *expr.node else {
        return;
    };
    let Some((name, suffix)) = method
        .as_str()
        .strip_suffix("_path")
        .map(|s| (s, "path"))
        .or_else(|| method.as_str().strip_suffix("_url").map(|s| (s, "url")))
    else {
        return;
    };
    let proxy = recv.as_ref().and_then(|recv| {
        if let ExprNode::Send {
            recv: None,
            method,
            args,
            block: None,
            ..
        } = &*recv.node
        {
            (args.is_empty() && !shadows.contains(method)).then_some(method.as_str())
        } else {
            None
        }
    });
    if proxy == Some("main_app") && owner.is_some() {
        *recv = Some(super::controller_to_library::rewrites::const_path(
            &["RouteHelpers"],
            expr.span,
        ));
        return;
    }
    let mount = if let Some(proxy) = proxy {
        unique_mount(mounts, |mount| mount.proxy == proxy)
    } else if recv.is_none() && !shadows.contains(method) {
        owner
    } else {
        None
    };
    if let Some(full_name) = mount.and_then(|mount| mount.names.get(name)) {
        *method = Symbol::from(format!("{full_name}_{suffix}"));
        *recv = Some(super::controller_to_library::rewrites::const_path(
            &["RouteHelpers"],
            expr.span,
        ));
    }
}
