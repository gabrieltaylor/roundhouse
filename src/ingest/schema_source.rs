use std::path::Path;

use ruby_prism::{Node, Visit};

use crate::schema::Schema;
use crate::vfs::Vfs;

use super::util::{constant_id_str, constant_path_of, symbol_value};
use super::{IngestError, IngestResult, survey};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Format {
    Ruby,
    Sql,
}

impl Format {
    fn path(self) -> &'static str {
        match self {
            Self::Ruby => "db/schema.rb",
            Self::Sql => "db/structure.sql",
        }
    }
}

#[derive(Default)]
struct Configuration {
    assignments: Vec<(usize, Option<Format>)>,
    unconditional: Vec<usize>,
}

impl Configuration {
    fn unresolved_write(&mut self, name: &str, receiver: Option<Node<'_>>, offset: usize) {
        if matches!(name, "schema_format" | "schema_format=") && active_record_config(receiver) {
            self.assignments.push((offset, None));
        }
    }
}

impl<'pr> Visit<'pr> for Configuration {
    fn visit_call_node(&mut self, call: &ruby_prism::CallNode<'pr>) {
        if constant_id_str(&call.name()) == "schema_format="
            && active_record_config(call.receiver())
        {
            let value = call
                .arguments()
                .and_then(|a| a.arguments().iter().next())
                .and_then(|n| symbol_value(&n));
            let format = match value.as_deref() {
                Some("ruby") => Some(Format::Ruby),
                Some("sql") => Some(Format::Sql),
                _ => None,
            };
            self.assignments
                .push((call.location().start_offset(), format));
        }
        ruby_prism::visit_call_node(self, call);
    }
    fn visit_call_or_write_node(&mut self, node: &ruby_prism::CallOrWriteNode<'pr>) {
        self.unresolved_write(
            constant_id_str(&node.write_name()),
            node.receiver(),
            node.location().start_offset(),
        );
        ruby_prism::visit_call_or_write_node(self, node);
    }

    fn visit_call_and_write_node(&mut self, node: &ruby_prism::CallAndWriteNode<'pr>) {
        self.unresolved_write(
            constant_id_str(&node.write_name()),
            node.receiver(),
            node.location().start_offset(),
        );
        ruby_prism::visit_call_and_write_node(self, node);
    }

    fn visit_call_operator_write_node(&mut self, node: &ruby_prism::CallOperatorWriteNode<'pr>) {
        self.unresolved_write(
            constant_id_str(&node.write_name()),
            node.receiver(),
            node.location().start_offset(),
        );
        ruby_prism::visit_call_operator_write_node(self, node);
    }

    fn visit_call_target_node(&mut self, node: &ruby_prism::CallTargetNode<'pr>) {
        self.unresolved_write(
            constant_id_str(&node.name()),
            Some(node.receiver()),
            node.location().start_offset(),
        );
        ruby_prism::visit_call_target_node(self, node);
    }
}

fn call_chain(node: &Node<'_>, path: &[&str]) -> bool {
    if path == ["Rails"] {
        return constant_path_of(node).is_some_and(|names| names == ["Rails"]);
    }
    let Some((name, receiver_path)) = path.split_last() else {
        return false;
    };
    let Some(call) = node.as_call_node() else {
        return false;
    };
    if constant_id_str(&call.name()) != *name
        || call.arguments().is_some()
        || call.block().is_some()
    {
        return false;
    }
    match call.receiver() {
        None => receiver_path.is_empty(),
        Some(receiver) if receiver_path.is_empty() => receiver.as_self_node().is_some(),
        Some(receiver) => call_chain(&receiver, receiver_path),
    }
}

fn active_record_config(receiver: Option<Node<'_>>) -> bool {
    receiver.is_some_and(|receiver| {
        call_chain(&receiver, &["config", "active_record"])
            || call_chain(
                &receiver,
                &["Rails", "application", "config", "active_record"],
            )
    })
}

fn unconditional_call(call: &ruby_prism::CallNode<'_>) -> bool {
    !call.is_safe_navigation()
        && call
            .receiver()
            .and_then(|node| node.as_call_node())
            .is_none_or(|receiver| unconditional_call(&receiver))
}

fn unconditional(node: Node<'_>, out: &mut Vec<usize>) {
    if let Some(p) = node.as_program_node() {
        unconditional(p.statements().as_node(), out);
    } else if let Some(s) = node.as_statements_node() {
        for n in s.body().iter() {
            unconditional(n, out);
        }
    } else if let Some(c) = node.as_class_node() {
        if let Some(body) = c.body() {
            unconditional(body, out);
        }
    } else if let Some(m) = node.as_module_node() {
        if let Some(body) = m.body() {
            unconditional(body, out);
        }
    } else if let Some(c) = node.as_call_node() {
        if constant_id_str(&c.name()) == "schema_format="
            && active_record_config(c.receiver())
            && unconditional_call(&c)
        {
            out.push(c.location().start_offset());
        }
        if constant_id_str(&c.name()) == "configure"
            && c.receiver()
                .is_some_and(|receiver| call_chain(&receiver, &["Rails", "application"]))
            && unconditional_call(&c)
        {
            if let Some(block) = c.block().and_then(|n| n.as_block_node()) {
                if let Some(body) = block.body() {
                    unconditional(body, out);
                }
            }
        }
    }
}

fn configured_format<V: Vfs + ?Sized>(
    vfs: &V,
    dir: &Path,
) -> IngestResult<Result<Option<Format>, String>> {
    let config_dir = dir.join("config");
    if !vfs.is_dir(&config_dir) {
        return Ok(Ok(None));
    }
    let mut formats = Vec::new();
    for path in super::app::read_rb_files(vfs, &config_dir)? {
        let source = vfs.read(&path)?;
        let parsed = ruby_prism::parse(&source);
        let mut config = Configuration::default();
        config.visit(&parsed.node());
        unconditional(parsed.node(), &mut config.unconditional);
        for (offset, format) in config.assignments {
            if !config.unconditional.contains(&offset)
                || format.is_none()
                || parsed.errors().next().is_some()
            {
                return Ok(Err(format!(
                    "cannot statically resolve schema_format in {}",
                    path.display()
                )));
            }
            formats.push(format.unwrap());
        }
    }
    if formats.iter().any(|f| Some(f) != formats.first()) {
        return Ok(Err(
            "conflicting schema_format settings (including environment configuration)".into(),
        ));
    }
    Ok(Ok(formats.first().copied()))
}

pub(super) fn ingest_app_schema<V: Vfs + ?Sized>(vfs: &V, dir: &Path) -> IngestResult<Schema> {
    let configured = configured_format(vfs, dir)?;
    let selection = match configured {
        Ok(Some(format)) => Some(format),
        Ok(None) => match (
            vfs.exists(&dir.join(Format::Ruby.path())),
            vfs.exists(&dir.join(Format::Sql.path())),
        ) {
            (true, false) => Some(Format::Ruby),
            (false, true) => Some(Format::Sql),
            (false, false) => None,
            (true, true) => {
                selection_gap(
                    dir,
                    "both db/schema.rb and db/structure.sql exist without a statically resolved schema_format",
                )?;
                None
            }
        },
        Err(reason) => {
            selection_gap(dir, &reason)?;
            None
        }
    };
    if let Some(format) = selection {
        let path = dir.join(format.path());
        if vfs.exists(&path) {
            let source = vfs.read(&path)?;
            let file = path.display().to_string();
            let result = match format {
                Format::Ruby => super::schema::ingest_schema(&source, &file),
                Format::Sql => super::structure_sql::ingest_structure_sql(&source, &file),
            };
            if let Some(schema) = survey::unwrap_or_record(result)? {
                if !source.iter().all(u8::is_ascii_whitespace)
                    && (format == Format::Ruby || !schema.tables.is_empty())
                {
                    return Ok(schema);
                }
            }
        } else if vfs.exists(&dir.join(match format {
            Format::Ruby => Format::Sql.path(),
            Format::Sql => Format::Ruby.path(),
        })) {
            selection_gap(
                dir,
                &format!("configured schema dump {} is missing", format.path()),
            )?;
        }
    }
    let mut schema = Schema::default();
    let migrations = dir.join("db/migrate");
    if vfs.is_dir(&migrations) {
        for path in super::app::read_rb_files(vfs, &migrations)? {
            survey::unwrap_or_record(super::schema::ingest_migration(
                &vfs.read(&path)?,
                &path.display().to_string(),
                &mut schema,
            ))?;
        }
    }
    Ok(schema)
}

fn selection_gap(dir: &Path, reason: &str) -> IngestResult<()> {
    survey::unwrap_or_record::<()>(Err(IngestError::Unsupported {
        file: dir.join("config/application.rb").display().to_string(),
        message: format!("schema source: {reason}; refusing to select a potentially stale dump; survey mode falls back to migrations"),
    })).map(|_| ())
}
