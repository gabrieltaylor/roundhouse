//! Source-local engines: discover declarations, then expose their source roots
//! to the ordinary Rails walkers. Ruby namespaces come from Ruby declarations;
//! file paths are never used to invent a namespace.

use std::collections::{BTreeMap, BTreeSet, HashMap, btree_map};
use std::io;
use std::path::{Path, PathBuf};

use ruby_prism::Node;

use super::app::read_rb_files;
use super::routes::{DrawSources, mounted_engine_constants};
use super::util::{
    class_name_path, constant_id_str, constant_path_segments_strs, find_all_classes_with_scope,
    flatten_statements, symbol_or_string_value,
};
use super::{IngestError, IngestResult};
use crate::vfs::Vfs;

/// Route source for an in-repository Rails engine that the app mounts.
/// The key in the map passed to the route ingester is
/// the mounted constant (`BlogEngine::Engine`).
#[derive(Debug)]
pub(super) struct LocalEngine {
    pub(super) source: Vec<u8>,
    pub(super) file: String,
    pub(super) draws: DrawSources,
    /// Namespace used by the engine's controller constants, e.g.
    /// `BlogEngine` for `BlogEngine::ArticlesController`.
    pub(super) module: Option<String>,
    pub(super) root: PathBuf,
    declaration_file: PathBuf,
    /// Prefix used to keep generated route helpers distinct from the
    /// host app's helpers (normally `blog_engine`).
    pub(super) helper_prefix: String,
    pub(super) table_prefix: Option<String>,
}

struct Declaration {
    namespace: Option<String>,
    helper_prefix: String,
    table_prefix: Option<String>,
}

fn engine_classes<'a>(node: &Node<'a>) -> Vec<(String, Vec<String>, ruby_prism::ClassNode<'a>)> {
    find_all_classes_with_scope(node).into_iter().filter_map(|(scope, class)| {
        let parent = class.superclass().and_then(|n| constant_path_segments_strs(&n));
        if !matches!(parent.as_deref(), Some([namespace, name]) if namespace == "Rails" && name == "Engine") {
            return None;
        }
        let mut nested = scope.clone();
        nested.extend(class_name_path(&class).unwrap_or_default());
        Some((nested.join("::"), scope, class))
    }).collect()
}

fn declaration(
    constant: &str,
    scope: &[String],
    class: &ruby_prism::ClassNode<'_>,
    file: &str,
) -> IngestResult<Declaration> {
    let mut namespace = None;
    let mut table_prefix = None;
    let mut helper_prefix = crate::naming::underscore(constant).replace('/', "_");
    for stmt in class.body().map(flatten_statements).unwrap_or_default() {
        let Some(call) = stmt.as_call_node() else {
            continue;
        };
        if call.receiver().is_some() {
            continue;
        }
        let call_name = call.name();
        let method = constant_id_str(&call_name);
        if !matches!(method, "isolate_namespace" | "engine_name") {
            continue;
        }
        let unsupported = || IngestError::Unsupported {
            file: file.into(),
            message: format!("local engine `{constant}` needs a literal `{method}`"),
        };
        let arg = call
            .arguments()
            .and_then(|a| a.arguments().iter().next())
            .ok_or_else(unsupported)?;
        if method == "engine_name" {
            helper_prefix = symbol_or_string_value(&arg).ok_or_else(unsupported)?;
        } else {
            let parts = constant_path_segments_strs(&arg).ok_or_else(unsupported)?;
            // Resolve a lexical namespace reference such as `Blog` inside
            // `module Acme; module Blog`, or an explicitly qualified path.
            let prefix: &[String] = if arg.location().as_slice().starts_with(b"::") {
                &[]
            } else if let Some(index) = scope.iter().position(|p| Some(p) == parts.first()) {
                &scope[..index]
            } else if parts.len() == 1 {
                &scope[..]
            } else {
                &[]
            };
            let name = prefix
                .iter()
                .chain(parts.iter())
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("::");
            helper_prefix = crate::naming::underscore(&name).replace('/', "_");
            // Rails captures this when isolation is declared; a later
            // engine_name changes the proxy but not the model table prefix.
            table_prefix = Some(format!("{helper_prefix}_"));
            namespace = Some(name);
        }
    }
    Ok(Declaration {
        namespace,
        helper_prefix,
        table_prefix,
    })
}

struct Candidate {
    root: PathBuf,
    file: PathBuf,
    source: Vec<u8>,
}

impl Candidate {
    fn load<V: Vfs + ?Sized>(self, vfs: &V, constant: &str) -> IngestResult<LocalEngine> {
        let file = self.file.display().to_string();
        let parsed = ruby_prism::parse(&self.source);
        if let Some(error) = parsed.errors().next() {
            return Err(IngestError::Parse {
                file,
                message: error.message().to_owned(),
            });
        }
        let (_, scope, class) = engine_classes(&parsed.node())
            .into_iter()
            .find(|(name, _, _)| name == constant)
            .expect("indexed engine declaration");
        let declaration = declaration(constant, &scope, &class, &file)?;
        let routes = self.root.join("config/routes.rb");
        Ok(LocalEngine {
            source: vfs.read(&routes)?,
            file: routes.display().to_string(),
            draws: read_draws(vfs, &self.root)?,
            root: self.root,
            declaration_file: self.file,
            module: declaration.namespace,
            helper_prefix: declaration.helper_prefix,
            table_prefix: declaration.table_prefix,
        })
    }
}

/// Conventional in-repository roots. Only engines reachable from mounts join
/// the app; an unused engine must not introduce controllers or models.
pub(super) fn discover<V: Vfs + ?Sized>(
    vfs: &V,
    dir: &Path,
) -> IngestResult<HashMap<String, LocalEngine>> {
    let host = dir.join("config/routes.rb");
    if !vfs.exists(&host) {
        return Ok(HashMap::new());
    }
    let mut candidates: HashMap<String, Vec<Candidate>> = HashMap::new();
    for container in ["engines", "components", "packs"] {
        let base = dir.join(container);
        if !vfs.is_dir(&base) {
            continue;
        }
        let mut roots = vfs.read_dir(&base)?;
        roots.sort();
        for root in roots {
            let file = root.join("config/routes.rb");
            if !vfs.is_dir(&root) || !vfs.exists(&file) {
                continue;
            }
            if !vfs.is_dir(&root.join("lib")) {
                continue;
            }
            for declaration_file in read_rb_files(vfs, &root.join("lib"))? {
                let bytes = vfs.read(&declaration_file)?;
                let parsed = ruby_prism::parse(&bytes);
                for (constant, _, _) in engine_classes(&parsed.node()) {
                    candidates.entry(constant).or_default().push(Candidate {
                        root: root.clone(),
                        file: declaration_file.clone(),
                        source: bytes.clone(),
                    });
                }
            }
        }
    }
    let mut pending = mounted_engine_constants(
        &vfs.read(&host)?,
        &host.display().to_string(),
        &read_draws(vfs, dir)?,
    );
    let mut engines = HashMap::new();
    while let Some(constant) = pending.pop() {
        let Some(mut found) = candidates.remove(&constant) else {
            continue;
        };
        if found.len() > 1 {
            return Err(IngestError::Unsupported {
                file: found[1].file.display().to_string(),
                message: format!("multiple local engines declare `{constant}`"),
            });
        }
        let engine = found
            .pop()
            .expect("indexed candidate")
            .load(vfs, &constant)?;
        pending.extend(mounted_engine_constants(
            &engine.source,
            &engine.file,
            &engine.draws,
        ));
        engines.insert(constant, engine);
    }
    Ok(engines)
}

pub(super) fn read_draws<V: Vfs + ?Sized>(vfs: &V, root: &Path) -> IngestResult<DrawSources> {
    let dir = root.join("config/routes");
    if !vfs.is_dir(&dir) {
        return Ok(HashMap::new());
    }
    read_rb_files(vfs, &dir)?
        .into_iter()
        .map(|path| {
            let name = path
                .strip_prefix(&dir)
                .expect("walked beneath routes")
                .with_extension("");
            Ok((
                name.to_string_lossy().replace('\\', "/"),
                (vfs.read(&path)?, path.display().to_string()),
            ))
        })
        .collect()
}

/// A read-through union of source paths. The host wins overrides; engine
/// collisions are rejected. No source bytes are copied, and existing
/// walkers remain responsible for choosing Ruby/template file extensions.
pub(super) struct EngineVfs<'a, V: Vfs + ?Sized> {
    base: &'a V,
    files: BTreeMap<PathBuf, PathBuf>,
    directories: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
}

impl<'a, V: Vfs + ?Sized> EngineVfs<'a, V> {
    pub(super) fn new(
        base: &'a V,
        dir: &Path,
        engines: &HashMap<String, LocalEngine>,
    ) -> IngestResult<Self> {
        let mut overlay = Self {
            base,
            files: BTreeMap::new(),
            directories: BTreeMap::new(),
        };
        let mut engines: Vec<_> = engines.values().collect();
        engines.sort_by(|a, b| a.root.cmp(&b.root));
        for engine in engines {
            for area in ["app", "lib"] {
                overlay.include(
                    &engine.root.join(area),
                    &dir.join(area),
                    &engine.declaration_file,
                )?;
            }
        }
        Ok(overlay)
    }

    fn resolve_path<'p>(&'p self, path: &'p Path) -> &'p Path {
        self.files.get(path).map_or(path, PathBuf::as_path)
    }

    fn include(
        &mut self,
        source: &Path,
        virtual_path: &Path,
        declaration: &Path,
    ) -> io::Result<()> {
        if source == declaration || !self.base.exists(source) {
            return Ok(());
        }
        if self.base.is_dir(source) {
            let mut children = self.base.read_dir(source)?;
            children.sort();
            for child in children {
                self.include(
                    &child,
                    &virtual_path.join(child.file_name().expect("directory entry has a file name")),
                    declaration,
                )?;
            }
        } else if !self.base.exists(virtual_path) {
            match self.files.entry(virtual_path.to_path_buf()) {
                btree_map::Entry::Occupied(entry) => {
                    let previous = entry.get();
                    if previous != source {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "local engine source collision at {}: {} and {}",
                                virtual_path.display(),
                                previous.display(),
                                source.display()
                            ),
                        ));
                    }
                    return Ok(());
                }
                btree_map::Entry::Vacant(entry) => {
                    entry.insert(source.to_path_buf());
                }
            }
            let mut child = virtual_path;
            while let Some(parent) = child.parent() {
                self.directories
                    .entry(parent.to_path_buf())
                    .or_default()
                    .insert(child.to_path_buf());
                child = parent;
            }
        }
        Ok(())
    }
}

impl<V: Vfs + ?Sized> Vfs for EngineVfs<'_, V> {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.base.read(self.resolve_path(path))
    }
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.base.read_to_string(self.resolve_path(path))
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries = if self.base.is_dir(path) {
            self.base.read_dir(path)?
        } else {
            Vec::new()
        };
        entries.extend(self.directories.get(path).into_iter().flatten().cloned());
        entries.sort();
        entries.dedup();
        Ok(entries)
    }
    fn exists(&self, path: &Path) -> bool {
        self.files.contains_key(path)
            || self.directories.contains_key(path)
            || self.base.exists(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.directories.contains_key(path) || self.base.is_dir(path)
    }
    fn source_path(&self, path: &Path) -> PathBuf {
        self.base.source_path(self.resolve_path(path))
    }
}
