use std::collections::HashMap;
use std::path::Path;

use crate::app::{App, HelperScope};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;

impl HelperScope {
    pub(crate) fn view_context_id(&self) -> ClassId {
        ClassId(Symbol::from(format!(
            "ActionView::Engine::{}",
            self.namespace
        )))
    }

    fn contains_name(&self, name: &str) -> bool {
        let name = name.strip_prefix("Views::").unwrap_or(name);
        name == self.namespace
            || name.starts_with(&format!("{}::", self.namespace))
            || name.starts_with(&format!("{}/", crate::naming::underscore(&self.namespace)))
    }

    pub(crate) fn find(scopes: &[Self], name: &str, source: Option<&Path>) -> Option<usize> {
        scopes
            .iter()
            .enumerate()
            .filter(|(_, scope)| scope.contains_name(name))
            .max_by_key(|(_, scope)| scope.namespace.len())
            .or_else(|| {
                scopes
                    .iter()
                    .enumerate()
                    .filter(|(_, scope)| source.is_some_and(|p| p.starts_with(&scope.source_root)))
                    .max_by_key(|(_, scope)| scope.source_root.len())
            })
            .map(|(index, _)| index)
    }
}

impl App {
    pub(crate) fn helper_scope_at(&self, name: &str, source: Option<&Path>) -> Option<usize> {
        HelperScope::find(&self.isolated_helper_scopes, name, source)
    }

    pub(crate) fn helper_scope_for(&self, name: &str, span: Span) -> Option<usize> {
        let source = span
            .file
            .0
            .checked_sub(1)
            .and_then(|index| self.sources.get(index as usize));
        self.helper_scope_at(name, source.map(|source| Path::new(&source.path)))
    }

    pub(crate) fn helper_methods_for(&self, name: &str, span: Span) -> &HashMap<Symbol, ClassId> {
        match self.helper_scope_for(name, span) {
            Some(index) => &self.isolated_helper_scopes[index].methods,
            None => &self.helper_method_index,
        }
    }

    pub(crate) fn helper_method_indices(&self) -> impl Iterator<Item = &HashMap<Symbol, ClassId>> {
        std::iter::once(&self.helper_method_index).chain(
            self.isolated_helper_scopes
                .iter()
                .map(|scope| &scope.methods),
        )
    }

    pub(crate) fn view_helper_contexts(
        &self,
    ) -> impl Iterator<Item = (ClassId, &HashMap<Symbol, ClassId>)> {
        std::iter::once((
            ClassId(Symbol::from("ActionView::Base")),
            &self.helper_method_index,
        ))
        .chain(
            self.isolated_helper_scopes
                .iter()
                .map(|scope| (scope.view_context_id(), &scope.methods)),
        )
    }

    pub(crate) fn view_context_id(&self, name: &str, span: Span) -> ClassId {
        match self.helper_scope_for(name, span) {
            Some(index) => self.isolated_helper_scopes[index].view_context_id(),
            None => ClassId(Symbol::from("ActionView::Base")),
        }
    }
}

pub(crate) fn for_each_body(
    app: &mut App,
    visit: &mut impl FnMut(Option<usize>, &mut crate::expr::Expr),
) {
    if app.isolated_helper_scopes.is_empty() {
        crate::lower::for_each_hook_body(app, &mut |body| visit(None, body));
        for view in &mut app.views {
            visit(None, &mut view.body);
        }
        return;
    }
    let scopes = app.isolated_helper_scopes.clone();
    let sources: Vec<_> = app
        .sources
        .iter()
        .map(|source| source.path.clone())
        .collect();
    let scope_for = |name: &str, span: Span| {
        let source = span
            .file
            .0
            .checked_sub(1)
            .and_then(|i| sources.get(i as usize));
        HelperScope::find(&scopes, name, source.map(Path::new))
    };
    crate::lower::for_each_hook_body_with_owner(app, &mut |owner, body| {
        visit(
            scope_for(owner.map_or("", |id| id.0.as_str()), body.span),
            body,
        );
    });
    for view in &mut app.views {
        visit(
            scope_for(view.name.as_str(), view.body.span),
            &mut view.body,
        );
    }
}
