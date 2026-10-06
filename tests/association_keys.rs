use std::path::{Path, PathBuf};
use std::process::Command;

use roundhouse::{Association, Model};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/association-keys")
}

fn app() -> roundhouse::App {
    roundhouse::ingest::ingest_app(&fixture()).unwrap()
}

fn model<'a>(app: &'a roundhouse::App, name: &str) -> &'a Model {
    app.models
        .iter()
        .find(|m| m.name.0.as_str() == name)
        .unwrap()
}

fn static_preload_source(app: &roundhouse::App) -> String {
    use roundhouse::lower::arel::{ArelVisitor, SqliteVisitor};
    use roundhouse::{Expr, ExprNode, Literal, Span, Symbol};
    let query = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: Some(Expr::new(
                Span::synthetic(),
                ExprNode::Const {
                    path: vec![Symbol::from("Ledger"), Symbol::from("Payment")],
                },
            )),
            method: Symbol::from("includes"),
            args: vec![Expr::new(
                Span::synthetic(),
                ExprNode::Lit {
                    value: Literal::Sym {
                        value: Symbol::from("entries"),
                    },
                },
            )],
            block: None,
            parenthesized: true,
        },
    );
    let analyzer = roundhouse::analyze::Analyzer::new(app);
    let graph = roundhouse::lower::model_associations::compute_association_graph(app);
    let (plan, owner) = roundhouse::lower::arel::try_build_arel_with_assocs(
        &query,
        &app.schema,
        analyzer.class_registry(),
        &graph,
    )
    .unwrap();
    roundhouse::emit::ruby::emit_expr(&SqliteVisitor.visit(&plan, &app.schema, &owner))
}

#[test]
fn static_preloads_keep_keys_types_and_scopes() {
    let source = static_preload_source(&app());
    for expected in [
        ".slug",
        "escape_string(key)",
        "payer_kind = 'Ledger::Payment'",
        "active = 1",
        "ORDER BY position ASC, id ASC",
    ] {
        assert!(source.contains(expected), "missing {expected}: {source}");
    }
}

#[test]
fn reflection_metadata_survives_ingestion() {
    let app = app();
    let payment = model(&app, "Ledger::Payment");
    assert_eq!(payment.table.0.as_str(), "receipts");
    for name in ["invoices", "bills", "audits"] {
        let assoc = payment
            .associations()
            .find(|a| a.name().as_str() == name)
            .unwrap();
        assert_eq!(
            assoc.target().0.as_str(),
            if name == "bills" {
                "Ledger::ArchivedInvoice"
            } else {
                "Ledger::Invoice"
            }
        );
        assert!(assoc.options().unwrap().source.is_some());
        assert!(matches!(
            assoc,
            Association::HasMany {
                through: Some(_),
                ..
            }
        ));
    }
    let entries = payment
        .associations()
        .find(|a| a.name().as_str() == "entries")
        .unwrap();
    assert_eq!(entries.primary_key().as_str(), "slug");
    let account = model(&app, "Ledger::Account");
    assert!(
        matches!(account.associations().next().unwrap(), Association::HasMany { foreign_key, target, .. }
        if foreign_key.as_str() == "account_id" && target.0.as_str() == "Ledger::Ticket")
    );
    let invoice = model(&app, "Ledger::Invoice");
    assert_eq!(
        invoice
            .associations()
            .next()
            .unwrap()
            .primary_key()
            .as_str(),
        "code"
    );
    assert_eq!(
        invoice.associations().nth(1).unwrap().target().0.as_str(),
        "Ledger::Payment"
    );
}

#[test]
fn generated_association_methods_parse() {
    let app = app();
    let (files, diagnostics) =
        roundhouse::emit::diagnostics::scope(|| roundhouse::emit::ruby::emit_lowered_models(&app));
    assert!(
        !diagnostics
            .iter()
            .any(|d| d.severity == roundhouse::diagnostic::Severity::Error
                && (d.message.contains("association") || d.message.contains("preload_generation"))),
        "{diagnostics:?}"
    );
    for file in files {
        if file.path.extension().and_then(|s| s.to_str()) != Some("rb") {
            continue;
        }
        let parsed = ruby_prism::parse(file.content.as_bytes());
        assert_eq!(
            parsed.errors().count(),
            0,
            "{}:\n{}",
            file.path.display(),
            file.content
        );
    }
}

#[test]
#[ignore = "requires Ruby, Active Record and sqlite3"]
fn generated_preloads_match_rails() {
    let app = app();
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/association-correctness/oracle");
    std::fs::create_dir_all(&scratch).unwrap();
    let files = roundhouse::emit::ruby::emit_lowered_models(&app);
    for file in files {
        let path = scratch.join(file.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, file.content).unwrap();
    }
    std::fs::write(
        scratch.join("static_preload.rb"),
        format!(
            "class Ledger::Payment\n  def self.static_batch\n{}\n  end\nend\n",
            static_preload_source(&app)
        ),
    )
    .unwrap();
    let output = Command::new("ruby")
        .arg("tests/support/association_keys_oracle.rb")
        .arg(fixture())
        .arg(&scratch)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unsupported_through_shapes_have_located_diagnostics() {
    let files = [
        ("db/schema.rb", "ActiveRecord::Schema.define do\n create_table(:owners) {}\n create_table(:links) {}\nend"),
        ("app/models/owner.rb", "class Owner < ApplicationRecord\n has_many :links\n has_many :items, through: 'links', source: 'missing'\n def self.batch; all.includes(:items); end\nend"),
        ("app/models/link.rb", "class Link < ApplicationRecord\nend"),
    ].into_iter().map(|(p, s)| (PathBuf::from(p), s.as_bytes().to_vec())).collect();
    let app = roundhouse::ingest::ingest_app_from_tree(files).unwrap();
    let diagnostics = roundhouse::analyze::diagnose(&app);
    let diagnostic = diagnostics
        .iter()
        .find(|d| d.message.contains("no source association missing"))
        .expect("missing source diagnostic");
    assert!(!diagnostic.span.is_synthetic());
    let (_, emitted) =
        roundhouse::emit::diagnostics::scope(|| roundhouse::emit::ruby::emit_lowered_models(&app));
    assert!(
        emitted
            .iter()
            .any(|d| d.message.contains("missing") && !d.span.is_synthetic())
    );
}

#[test]
fn qualified_scope_hashes_keep_the_table_key() {
    let app = app();
    let owner = model(&app, "Ledger::Entry");
    let assoc = owner
        .associations()
        .find(|assoc| assoc.name().as_str() == "restricted_invoice")
        .unwrap();
    let plan = roundhouse::lower::association_plan::resolve(&app.models, owner, assoc).unwrap();
    let roundhouse::ExprNode::Send { args, .. } = &*plan.scopes[0].node else {
        panic!("expected a where scope");
    };
    let roundhouse::ExprNode::Hash { entries, .. } = &*args[0].node else {
        panic!("expected table-qualified conditions");
    };
    assert!(matches!(&*entries[0].0.node,
        roundhouse::ExprNode::Lit { value: roundhouse::Literal::Sym { value } }
        if value.as_str() == "documents"));
}

#[test]
fn intermediate_preloads_are_not_retargeted_to_the_final_model() {
    let mut app = app();
    let owner = app
        .models
        .iter_mut()
        .find(|model| model.name.0.as_str() == "Ledger::Payment")
        .unwrap();
    for item in &mut owner.body {
        if let roundhouse::ModelBodyItem::Association {
            assoc: Association::HasMany { name, scope, .. },
            ..
        } = item
        {
            if name.as_str() == "entries" {
                let parsed = ruby_prism::parse(b"includes(:invoice)");
                let call = parsed
                    .node()
                    .as_program_node()
                    .unwrap()
                    .statements()
                    .body()
                    .iter()
                    .next()
                    .unwrap();
                *scope = Some(roundhouse::ingest::ingest_expr(&call, "scope.rb").unwrap());
            }
        }
    }
    let diagnostics = roundhouse::analyze::diagnose(&app);
    assert!(
        diagnostics.iter().any(
            |diagnostic| diagnostic.message.contains("through intermediate")
                && !diagnostic.span.is_synthetic()
        ),
        "{diagnostics:?}"
    );
}

#[test]
fn dynamic_polymorphic_type_columns_are_diagnosed() {
    let files = [
        ("db/schema.rb", "ActiveRecord::Schema.define do\n create_table(:owners) {}\n create_table(:items) {}\nend"),
        ("app/models/owner.rb", "class Owner < ApplicationRecord\n has_many :items, as: :attachable, foreign_type: type_column\nend"),
        ("app/models/item.rb", "class Item < ApplicationRecord\n belongs_to :attachable, polymorphic: true, foreign_type: type_column\nend"),
    ]
    .into_iter()
    .map(|(path, source)| (PathBuf::from(path), source.as_bytes().to_vec()))
    .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(files).unwrap();
    let diagnostics = roundhouse::analyze::diagnose(&app);
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.message.contains("non-literal foreign_type"))
        .collect();
    assert_eq!(errors.len(), 2, "{diagnostics:?}");
    assert!(
        errors
            .iter()
            .all(|diagnostic| !diagnostic.span.is_synthetic())
    );
}

#[test]
fn analysis_uses_the_resolved_through_target() {
    let mut app = app();
    roundhouse::analyze::Analyzer::new(&app).analyze(&mut app);
    let payment = model(&app, "Ledger::Payment");
    let invoices = payment
        .associations()
        .find(|a| a.name().as_str() == "invoices")
        .unwrap();
    assert_eq!(invoices.target().0.as_str(), "Ledger::Invoice");
    let codes = payment
        .methods()
        .find(|m| m.name.as_str() == "invoice_codes")
        .unwrap();
    assert!(
        matches!(codes.body.ty.as_ref(), Some(roundhouse::Ty::Array { elem }) if **elem == roundhouse::Ty::Str),
        "{:?}",
        codes.body.ty
    );
    assert!(
        roundhouse::analyze::diagnose(&app)
            .iter()
            .all(|d| !d.message.contains("Invoice#") || !d.message.contains("unknown"))
    );
}
