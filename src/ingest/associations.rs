use crate::dialect::{Association, Model, ModelBodyItem};
use crate::ident::{ClassId, Symbol};

pub(crate) fn qualify(models: &[Model], owner: &ClassId, target: &ClassId) -> ClassId {
    let name = target.0.as_str();
    if let Some(absolute) = name.strip_prefix("::") {
        return ClassId(Symbol::from(absolute));
    }
    let mut scope = owner.0.as_str();
    loop {
        let candidate = ClassId(Symbol::from(format!("{scope}::{name}")));
        if models.iter().any(|m| m.name == candidate) {
            return candidate;
        }
        let Some((parent, _)) = scope.rsplit_once("::") else {
            break;
        };
        scope = parent;
    }
    target.clone()
}

pub(crate) fn source_association<'a>(
    intermediate: &'a Model,
    assoc: &Association,
) -> Result<&'a Association, String> {
    let source = assoc.options().and_then(|o| o.source.as_ref());
    let singular = crate::naming::singularize(assoc.name().as_str());
    let candidates: Vec<_> = intermediate
        .associations()
        .filter(|a| match source {
            Some(name) => a.name() == name,
            None => a.name() == assoc.name() || a.name().as_str() == singular,
        })
        .collect();
    match candidates.as_slice() {
        [source] => Ok(source),
        [] => Err(format!(
            "no source association {} on {}",
            source.unwrap_or(assoc.name()),
            intermediate.name.0
        )),
        _ => Err(format!(
            "ambiguous source on {}; specify source:",
            intermediate.name.0
        )),
    }
}

pub(crate) fn resolve_target(
    models: &[Model],
    owner: &Model,
    assoc: &Association,
    depth: usize,
) -> Result<ClassId, String> {
    if depth > 16 {
        return Err("cyclic or excessively deep through association".into());
    }
    let Association::HasMany {
        through: Some(through),
        options,
        ..
    } = assoc
    else {
        return Ok(qualify(models, &owner.name, assoc.target()));
    };
    let hop = owner
        .associations()
        .find(|a| a.name() == through)
        .ok_or_else(|| format!("missing through association {through} on {}", owner.name.0))?;
    let intermediate = resolve_target(models, owner, hop, depth + 1)?;
    let intermediate = models
        .iter()
        .find(|m| m.name == intermediate)
        .ok_or_else(|| format!("through model {} is unavailable", intermediate.0))?;
    let source = source_association(intermediate, assoc)?;
    if matches!(
        source,
        Association::BelongsTo {
            polymorphic: true,
            ..
        }
    ) {
        let target = options
            .source_type
            .as_ref()
            .ok_or_else(|| "polymorphic through source requires source_type:".to_string())?;
        return Ok(qualify(
            models,
            &owner.name,
            options.class_name.as_ref().unwrap_or(target),
        ));
    }
    if options.source_type.is_some() {
        return Err("source_type: requires a polymorphic belongs_to source".into());
    }
    if let Some(target) = &options.class_name {
        return Ok(qualify(models, &owner.name, target));
    }
    resolve_target(models, intermediate, source, depth + 1)
}

pub(crate) fn resolve(app: &mut crate::App) {
    let updates: Vec<_> = app
        .models
        .iter()
        .map(|model| {
            model
                .associations()
                .map(|assoc| {
                    let target = resolve_target(&app.models, model, assoc, 0);
                    let key_owner = match assoc {
                        Association::BelongsTo { .. } => target
                            .as_ref()
                            .ok()
                            .and_then(|id| app.models.iter().find(|m| &m.name == id)),
                        _ => Some(model),
                    };
                    let key = key_owner
                        .and_then(|m| {
                            m.primary_key.clone().or_else(|| {
                                app.schema.tables.get(&m.table.0).and_then(|t| {
                                    t.columns
                                        .iter()
                                        .find(|c| c.primary_key)
                                        .map(|c| c.name.clone())
                                })
                            })
                        })
                        .unwrap_or_else(|| Symbol::from("id"));
                    (target, key)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    for (model, updates) in app.models.iter_mut().zip(updates) {
        let assocs = model.body.iter_mut().filter_map(|item| match item {
            ModelBodyItem::Association { assoc, .. } => Some(assoc),
            _ => None,
        });
        for (assoc, (resolved, key)) in assocs.zip(updates) {
            match assoc {
                Association::BelongsTo {
                    target, options, ..
                }
                | Association::HasMany {
                    target, options, ..
                }
                | Association::HasOne {
                    target, options, ..
                } => {
                    match resolved {
                        Ok(id) => *target = id,
                        Err(error) => options.unsupported.push(error),
                    }
                    options.primary_key.get_or_insert(key);
                }
                Association::HasAndBelongsToMany { target, .. } => {
                    if let Ok(id) = resolved {
                        *target = id;
                    }
                }
            }
        }
    }
}
