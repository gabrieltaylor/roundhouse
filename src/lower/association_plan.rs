use crate::dialect::{Association, Model};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;

#[derive(Clone, Debug)]
pub struct AssociationPlan {
    pub target: ClassId,
    pub table: String,
    pub owner_key: Symbol,
    pub target_primary_key: Symbol,
    pub group_column: String,
    pub joins: Vec<String>,
    pub owner_join: String,
    pub conditions: Vec<(String, String)>,
    pub scopes: Vec<Expr>,
    pub preloads: Vec<Expr>,
}

struct Hop {
    target: ClassId,
    table: String,
    owner_key: Symbol,
    target_key: Symbol,
    record_key: Symbol,
}

struct Path {
    hops: Vec<Hop>,
    conditions: Vec<(String, String)>,
    scopes: Vec<Expr>,
}

#[derive(Clone, Copy)]
struct ConcreteTarget<'a> {
    class: &'a ClassId,
    polymorphic_type: Option<&'a str>,
}

pub fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
}

pub(crate) fn referenced_key(assoc: &Association, target: &Model) -> Symbol {
    if assoc
        .options()
        .is_some_and(|options| options.primary_key_explicit)
    {
        assoc.primary_key()
    } else {
        target
            .primary_key
            .clone()
            .unwrap_or_else(|| Symbol::from("id"))
    }
}

pub fn resolve(
    models: &[Model],
    owner: &Model,
    assoc: &Association,
) -> Result<AssociationPlan, String> {
    let path = resolve_path(models, owner, assoc, None, 0)?;
    let first = path.hops.first().ok_or("empty association path")?;
    let last = path.hops.last().unwrap();
    let mut tables = std::collections::HashSet::new();
    if path.hops.iter().any(|h| !tables.insert(&h.table)) {
        return Err("through joins requiring table aliases are not supported".into());
    }
    let joins = path
        .hops
        .windows(2)
        .rev()
        .map(|pair| {
            let [from, to] = pair else { unreachable!() };
            format!(
                "INNER JOIN {} ON {}.{} = {}.{}",
                from.table, from.table, to.owner_key, to.table, to.target_key
            )
        })
        .collect();
    let mut owner_join = format!(
        "{} ON {}.{} = {}.{}",
        first.table, first.table, first.target_key, owner.table.0, first.owner_key
    );
    for pair in path.hops.windows(2) {
        let [from, to] = pair else { unreachable!() };
        owner_join.push_str(&format!(
            " INNER JOIN {} ON {}.{} = {}.{}",
            to.table, to.table, to.target_key, from.table, to.owner_key
        ));
    }
    for (column, value) in &path.conditions {
        owner_join.push_str(&format!(" AND {column} = '{}'", value.replace('\'', "''")));
    }
    let mut preloads = Vec::new();
    fn collect_preloads(expr: &Expr, out: &mut Vec<Expr>) {
        if let ExprNode::Send { method, args, .. } = &*expr.node {
            if matches!(method.as_str(), "includes" | "preload" | "eager_load") {
                out.extend(args.iter().cloned());
            }
        }
        expr.node
            .for_each_child(&mut |child| collect_preloads(child, out));
    }
    for scope in &path.scopes {
        collect_preloads(scope, &mut preloads);
    }
    Ok(AssociationPlan {
        target: last.target.clone(),
        table: last.table.clone(),
        owner_key: first.owner_key.clone(),
        target_primary_key: last.record_key.clone(),
        group_column: format!("{}.{}", first.table, first.target_key),
        joins,
        owner_join,
        conditions: path.conditions,
        scopes: path.scopes,
        preloads,
    })
}

fn resolve_path(
    models: &[Model],
    owner: &Model,
    assoc: &Association,
    concrete: Option<ConcreteTarget<'_>>,
    depth: usize,
) -> Result<Path, String> {
    if depth > 16 {
        return Err("cyclic or excessively deep through association".into());
    }
    if let Some(options) = assoc.options() {
        if !options.unsupported.is_empty() {
            return Err(options.unsupported.join("; "));
        }
    }
    if !identifier(assoc.name().as_str()) {
        return Err("association name is not a supported Ruby accessor".into());
    }
    if let Association::HasMany {
        through: Some(through),
        options,
        ..
    } = assoc
    {
        let via = owner
            .associations()
            .find(|a| a.name() == through)
            .ok_or_else(|| format!("missing through association {through}"))?;
        let mut first = resolve_path(models, owner, via, None, depth + 1)?;
        let intermediate = models
            .iter()
            .find(|m| m.name == first.hops.last().unwrap().target)
            .ok_or("through model is unavailable")?;
        let source = crate::ingest::associations::source_association(intermediate, assoc)?;
        let concrete = options
            .source_type
            .as_ref()
            .or(options.class_name.as_ref())
            .map(|_| ConcreteTarget {
                class: assoc.target(),
                polymorphic_type: options.source_type.as_ref().map(|t| t.0.as_str()),
            });
        let mut last = resolve_path(models, intermediate, source, concrete, depth + 1)?;
        let target_table = last.hops.last().unwrap().table.clone();
        first.hops.append(&mut last.hops);
        first.conditions.append(&mut last.conditions);
        let mut scopes = qualified_scope(assoc.scope(), &target_table)?;
        scopes.append(&mut last.scopes);
        scopes.append(&mut first.scopes);
        first.scopes = scopes;
        return Ok(first);
    }
    let target_id = concrete.map(|c| c.class).unwrap_or(assoc.target());
    let target = models
        .iter()
        .find(|m| &m.name == target_id)
        .ok_or_else(|| format!("association target {} is unavailable", target_id.0))?;
    let table = target.table.0.as_str().to_string();
    if !table.split('.').all(identifier) {
        return Err(format!("unsupported table identifier {table:?}"));
    }
    let mut conditions = Vec::new();
    let (owner_key, target_key) = match assoc {
        Association::BelongsTo {
            foreign_key,
            polymorphic,
            options,
            name,
            ..
        } => {
            if *polymorphic {
                if concrete.is_none() {
                    return Err(
                        "polymorphic belongs_to needs a concrete source_type for batching".into(),
                    );
                }
                let type_key = options
                    .foreign_type
                    .clone()
                    .unwrap_or_else(|| Symbol::from(format!("{name}_type")));
                conditions.push((
                    format!("{}.{}", owner.table.0, type_key),
                    concrete
                        .and_then(|c| c.polymorphic_type)
                        .unwrap_or(target.name.0.as_str())
                        .to_string(),
                ));
            }
            let primary_key = if concrete.is_some() {
                referenced_key(assoc, target)
            } else {
                assoc.primary_key()
            };
            (foreign_key.clone(), primary_key)
        }
        Association::HasMany {
            foreign_key,
            as_interface,
            options,
            ..
        }
        | Association::HasOne {
            foreign_key,
            as_interface,
            options,
            ..
        } => {
            if let Some(interface) = as_interface {
                let type_key = options
                    .foreign_type
                    .clone()
                    .unwrap_or_else(|| Symbol::from(format!("{interface}_type")));
                conditions.push((
                    format!("{table}.{type_key}"),
                    owner.name.0.as_str().to_string(),
                ));
            }
            (assoc.primary_key(), foreign_key.clone())
        }
        Association::HasAndBelongsToMany { .. } => {
            return Err("HABTM query planning is not supported".into());
        }
    };
    if !identifier(owner_key.as_str()) || !identifier(target_key.as_str()) {
        return Err("composite or non-identifier association keys are not supported".into());
    }
    let record_key = if matches!(assoc, Association::BelongsTo { .. }) {
        target_key.clone()
    } else {
        target
            .primary_key
            .clone()
            .unwrap_or_else(|| Symbol::from("id"))
    };
    Ok(Path {
        hops: vec![Hop {
            target: target.name.clone(),
            table: table.clone(),
            owner_key,
            target_key,
            record_key,
        }],
        conditions,
        scopes: qualified_scope(assoc.scope(), &table)?,
    })
}

fn qualified_scope(scope: Option<&Expr>, table: &str) -> Result<Vec<Expr>, String> {
    let Some(scope) = scope else {
        return Ok(vec![]);
    };
    fn qualify(expr: &Expr, table: &str) -> Result<Expr, String> {
        let ExprNode::Send {
            recv,
            method,
            args,
            block: None,
            parenthesized,
        } = &*expr.node
        else {
            return Err("association scope must be a query chain".into());
        };
        if !matches!(
            method.as_str(),
            "where" | "order" | "reorder" | "distinct" | "includes" | "preload" | "eager_load"
        ) {
            return Err(format!(
                "association scope operation {method} is not supported for shared loading"
            ));
        }
        let mut args = args.clone();
        for arg in &mut args {
            if matches!(method.as_str(), "order" | "reorder") {
                if let ExprNode::Lit {
                    value: Literal::Sym { value },
                } = &*arg.node
                {
                    *arg = string(&format!("{table}.{value}"));
                }
            }
            if matches!(method.as_str(), "includes" | "preload" | "eager_load") {
                continue;
            }
            if let ExprNode::Hash { entries, .. } = &mut *arg.node {
                for (key, _) in entries {
                    let name = match &*key.node {
                        ExprNode::Lit {
                            value: Literal::Sym { value },
                        } => value.as_str(),
                        ExprNode::Lit {
                            value: Literal::Str { value },
                        } => value.as_str(),
                        _ => return Err("dynamic association scope key".into()),
                    };
                    if !name.contains('.') {
                        *key = string(&format!("{table}.{name}"));
                    }
                }
            }
        }
        Ok(Expr::new(
            expr.span,
            ExprNode::Send {
                recv: recv.as_ref().map(|r| qualify(r, table)).transpose()?,
                method: method.clone(),
                args,
                block: None,
                parenthesized: *parenthesized,
            },
        ))
    }
    Ok(vec![qualify(scope, table)?])
}

impl AssociationPlan {
    pub fn query(&self, keys: Expr) -> Expr {
        let target = Expr::new(
            Span::synthetic(),
            ExprNode::Const {
                path: self
                    .target
                    .0
                    .as_str()
                    .split("::")
                    .map(Symbol::from)
                    .collect(),
            },
        );
        let relation = Expr::new(
            Span::synthetic(),
            ExprNode::Const {
                path: vec![Symbol::from("ActiveRecord"), Symbol::from("Relation")],
            },
        );
        let mut query = send(relation, "new", vec![target]);
        if !self.joins.is_empty() {
            query = send(query, "joins", vec![string(&self.joins.join(" "))]);
        }
        let mut entries = Vec::new();
        if matches!(&*keys.node, ExprNode::Ivar { .. }) {
            query = send(
                query,
                "where",
                vec![string(&format!("{} = ?", self.group_column)), keys],
            );
        } else {
            entries.push((string(&self.group_column), keys));
        }
        entries.extend(
            self.conditions
                .iter()
                .map(|(key, value)| (string(key), string(value))),
        );
        if !entries.is_empty() {
            query = send(
                query,
                "where",
                vec![Expr::new(
                    Span::synthetic(),
                    ExprNode::Hash {
                        entries,
                        kwargs: false,
                    },
                )],
            );
        }
        for scope in &self.scopes {
            query = graft(scope, query);
        }
        query
    }

    pub fn reader(&self, name: &Symbol) -> Expr {
        let ivar = |name: Symbol| Expr::new(Span::synthetic(), ExprNode::Ivar { name });
        send(
            self.query(ivar(self.owner_key.clone())),
            "preloaded",
            vec![
                ivar(Symbol::from(format!("{name}_cache"))),
                ivar(Symbol::from(format!("{name}_loaded"))),
            ],
        )
    }
}

pub(crate) fn graft(scope: &Expr, seed: Expr) -> Expr {
    let ExprNode::Send {
        recv,
        method,
        args,
        block,
        parenthesized,
    } = &*scope.node
    else {
        return seed;
    };
    Expr::new(
        scope.span,
        ExprNode::Send {
            recv: Some(match recv {
                Some(r) => graft(r, seed),
                None => seed,
            }),
            method: method.clone(),
            args: args.clone(),
            block: block.clone(),
            parenthesized: *parenthesized,
        },
    )
}

fn string(value: &str) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Lit {
            value: Literal::Str {
                value: value.to_string(),
            },
        },
    )
}

fn send(recv: Expr, method: &str, args: Vec<Expr>) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(recv),
            method: Symbol::from(method),
            args,
            block: None,
            parenthesized: true,
        },
    )
}
