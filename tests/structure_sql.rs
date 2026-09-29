use std::collections::HashMap;
use std::path::PathBuf;

use roundhouse::Symbol;
use roundhouse::ingest::{
    ingest_app_from_tree, ingest_model, ingest_schema, structure_sql::ingest_structure_sql, survey,
};
use roundhouse::schema::{ColumnType, ReferentialAction};

const DUMP: &str = include_str!("../fixtures/postgres-blog/db/structure.sql");

fn parse(sql: &str) -> roundhouse::schema::Schema {
    ingest_structure_sql(sql.as_bytes(), "db/structure.sql").unwrap()
}

fn tree(files: &[(&str, &str)]) -> HashMap<PathBuf, Vec<u8>> {
    files
        .iter()
        .map(|(p, s)| (PathBuf::from(p), s.as_bytes().to_vec()))
        .collect()
}

#[test]
fn pg_dump_facts_reach_shared_schema() {
    let schema = parse(DUMP);
    let table = &schema.tables[&Symbol::from("articles")];
    assert_eq!(table.columns.len(), 17);
    let col = |name: &str| {
        table
            .columns
            .iter()
            .find(|c| c.name.as_str() == name)
            .unwrap()
    };
    assert!(col("id").primary_key && !col("id").nullable);
    assert!(col("id").default.is_none());
    assert_eq!(
        col("title").col_type,
        ColumnType::String { limit: Some(255) }
    );
    assert!(
        col("title")
            .default
            .as_ref()
            .unwrap()
            .contains("a; title, with -- punctuation")
    );
    assert_eq!(
        col("amount").col_type,
        ColumnType::Decimal {
            precision: Some(12),
            scale: Some(2)
        }
    );
    assert_eq!(col("state").col_type, ColumnType::String { limit: None });
    assert_eq!(col("active").default.as_deref(), Some("true"));
    assert_eq!(col("payload").col_type, ColumnType::Json);
    assert_eq!(col("token").col_type, ColumnType::Uuid);
    assert_eq!(col("bytes").col_type, ColumnType::Binary);
    assert_eq!(col("created_at").col_type, ColumnType::DateTime);
    assert!(!col("created_at").nullable);
    assert!(col("updated_at").nullable);
    assert_eq!(table.indexes.len(), 2);
    assert!(table.indexes[1].unique);
    assert_eq!(table.foreign_keys[0].to_table.0.as_str(), "authors");
    assert_eq!(table.foreign_keys[0].on_update, ReferentialAction::Cascade);
    assert_eq!(table.foreign_keys[0].on_delete, ReferentialAction::Restrict);
    assert_eq!(table.check_constraints.len(), 2);
    let pg = schema.postgresql.unwrap();
    assert_eq!(
        pg.enums[&Symbol::from("article_state")],
        ["draft", "published", "editor's choice"]
    );
    assert!(pg.declarations.iter().any(|s| s.contains("OWNED BY")));
    assert!(pg.declarations.iter().any(|s| s.contains("IDENTITY")));
    assert!(pg.declarations.iter().any(|s| s.contains("nextval")));
}

#[test]
fn ruby_and_sql_produce_the_same_model_attributes() {
    let ruby = ingest_schema(
        br#"ActiveRecord::Schema[8.0].define do
      create_table "articles" do |t|
        t.bigint "author_id", null: false
        t.string "title", null: false, limit: 255
        t.text "body"
        t.enum "state", null: false, enum_type: "article_state"
        t.boolean "active", null: false
        t.integer "score", null: false
        t.decimal "amount", precision: 12, scale: 2
        t.float "ratio"
        t.uuid "token"
        t.jsonb "payload", null: false
        t.binary "bytes"
        t.date "published_on"
        t.time "starts_at"
        t.datetime "created_at", null: false
        t.timestamptz "updated_at"
        t.inet "address"
      end
    end"#,
        "equivalent_schema.rb",
    )
    .unwrap();
    let sql = parse(DUMP);
    let source = b"class Article < ApplicationRecord; end";
    let model = |schema| {
        ingest_model(source, "app/models/article.rb", schema, &Default::default())
            .unwrap()
            .unwrap()
    };
    assert_eq!(model(&ruby).attributes, model(&sql).attributes);
}

#[test]
fn functions_triggers_and_unknown_ddl_are_gaps_without_losing_columns() {
    let sql = format!(
        "{DUMP}\n\
      CREATE FUNCTION public.touch_article() RETURNS trigger LANGUAGE plpgsql AS $body$\n\
      BEGIN NEW.updated_at = now(); /* ; */ RETURN NEW; END; $body$;\n\
      CREATE TRIGGER touch_article BEFORE UPDATE ON public.articles FOR EACH ROW EXECUTE FUNCTION public.touch_article();\n\
      ALTER TABLE public.articles ENABLE ROW LEVEL SECURITY;\n\
      CREATE VIEW public.titles AS SELECT title FROM public.articles;"
    );
    assert!(
        ingest_structure_sql(sql.as_bytes(), "db/structure.sql")
            .unwrap_err()
            .to_string()
            .contains("CREATE FUNCTION")
    );
    survey::activate();
    let schema = parse(&sql);
    let gaps = survey::drain();
    assert_eq!(gaps.len(), 4, "{gaps:?}");
    assert_eq!(schema.tables[&Symbol::from("articles")].columns.len(), 17);
    assert!(gaps.iter().all(|g| g.to_string().contains("line ")));
}

#[test]
fn source_selection_respects_configuration_and_lone_dumps() {
    let ruby = "ActiveRecord::Schema.define do; create_table :ruby_records; end";
    let sql = "CREATE TABLE sql_records (id bigint PRIMARY KEY)";
    for (config, expected) in [
        ("config.active_record.schema_format = :sql", "sql_records"),
        ("config.active_record.schema_format = :ruby", "ruby_records"),
    ] {
        let app = ingest_app_from_tree(tree(&[
            ("db/schema.rb", ruby),
            ("db/structure.sql", sql),
            ("config/application.rb", config),
        ]))
        .unwrap();
        assert!(app.schema.tables.contains_key(&Symbol::from(expected)));
        assert_eq!(app.schema.tables.len(), 1);
    }
    for (path, src, expected) in [
        ("db/schema.rb", ruby, "ruby_records"),
        ("db/structure.sql", sql, "sql_records"),
    ] {
        let app = ingest_app_from_tree(tree(&[(path, src)])).unwrap();
        assert!(app.schema.tables.contains_key(&Symbol::from(expected)));
    }
}

#[test]
fn ambiguous_sources_report_gaps_and_survey_folds_migrations() {
    for config in [
        "",
        "config.active_record.schema_format = ENV.fetch('SCHEMA_FORMAT').to_sym",
        "config.active_record.schema_format = :sql if ENV['SQL']",
    ] {
        let files = tree(&[
            (
                "db/schema.rb",
                "ActiveRecord::Schema.define do; create_table :stale; end",
            ),
            ("db/structure.sql", "CREATE TABLE also_stale (id bigint)"),
            ("config/application.rb", config),
            (
                "db/migrate/001_create.rb",
                "class CreateFresh < ActiveRecord::Migration[8.0]; def change; create_table :fresh; end; end",
            ),
        ]);
        assert!(
            ingest_app_from_tree(files.clone())
                .unwrap_err()
                .to_string()
                .contains("schema source")
        );
        survey::activate();
        let app = ingest_app_from_tree(files).unwrap();
        let gaps = survey::drain();
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!(app.schema.tables.contains_key(&Symbol::from("fresh")));
        assert_eq!(app.schema.tables.len(), 1);
    }
}

#[test]
fn quoted_names_serial_and_literals_survive_statement_splitting() {
    let schema = parse(
        r#"/* outer /* nested */ comment */
        CREATE TABLE public."Odd;Table" (
            "Key" serial PRIMARY KEY,
            "Label" text DEFAULT 'it''s; still one literal',
            bytes bytea DEFAULT E'\\x41',
            enabled bool DEFAULT false,
            code uuid UNIQUE
        );
        CREATE TABLE archive.records (id int8 NOT NULL, note text);
        CREATE INDEX "Odd;Index" ON public."Odd;Table" ("Label");"#,
    );
    let table = &schema.tables[&Symbol::from("Odd;Table")];
    assert_eq!(table.columns[0].name.as_str(), "Key");
    assert!(!table.columns[0].nullable);
    assert_eq!(
        table.columns[1].default.as_deref(),
        Some("it's; still one literal")
    );
    assert!(schema.tables.contains_key(&Symbol::from("archive.records")));
    assert_eq!(table.indexes.len(), 2);
}

#[test]
fn configuration_conflicts_missing_dumps_and_compound_writes_are_explicit() {
    let migration = "class CreateFresh < ActiveRecord::Migration[8.0]; def change; create_table :fresh; end; end";
    let mut files = tree(&[
        (
            "config/application.rb",
            "module Demo; class Application < Rails::Application; config.active_record.schema_format = :sql; end; end",
        ),
        ("db/migrate/001_create.rb", migration),
    ]);
    assert!(
        ingest_app_from_tree(files.clone())
            .unwrap()
            .schema
            .tables
            .contains_key(&Symbol::from("fresh"))
    );
    files.insert(
        "db/schema.rb".into(),
        b"ActiveRecord::Schema.define do; create_table :stale; end".to_vec(),
    );
    assert!(
        ingest_app_from_tree(files.clone())
            .unwrap_err()
            .to_string()
            .contains("db/structure.sql is missing")
    );
    files.insert(
        "db/structure.sql".into(),
        b"CREATE TABLE current (id bigint)".to_vec(),
    );
    assert!(
        ingest_app_from_tree(files.clone())
            .unwrap()
            .schema
            .tables
            .contains_key(&Symbol::from("current"))
    );
    files.insert(
        "config/environments/production.rb".into(),
        b"Rails.application.configure do; config.active_record.schema_format = :ruby; end".to_vec(),
    );
    assert!(
        ingest_app_from_tree(files)
            .unwrap_err()
            .to_string()
            .contains("conflicting schema_format")
    );
    let error = ingest_app_from_tree(tree(&[
        (
            "config/application.rb",
            "config.active_record.schema_format ||= :sql",
        ),
        ("db/schema.rb", "ActiveRecord::Schema.define do; end"),
    ]))
    .unwrap_err();
    assert!(
        error.to_string().contains("cannot statically resolve"),
        "{error}"
    );
}

#[test]
fn empty_and_malformed_dumps_have_a_migration_fallback() {
    let migration = "class CreateFresh < ActiveRecord::Migration[8.0]; def change; create_table :fresh; end; end";
    for empty in [
        "",
        "  \n",
        "-- only a dump header\nSET statement_timeout = 0;",
    ] {
        let app = ingest_app_from_tree(tree(&[
            ("db/structure.sql", empty),
            ("db/migrate/001_create.rb", migration),
        ]))
        .unwrap();
        assert!(app.schema.tables.contains_key(&Symbol::from("fresh")));
    }
    survey::activate();
    let app = ingest_app_from_tree(tree(&[
        (
            "db/structure.sql",
            "CREATE TABLE broken (x text DEFAULT 'unterminated);",
        ),
        ("db/migrate/001_create.rb", migration),
    ]))
    .unwrap();
    let gaps = survey::drain();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(app.schema.tables.contains_key(&Symbol::from("fresh")));
}

#[test]
fn a_partial_dump_is_not_overwritten_by_migration_folding() {
    survey::activate();
    let app = ingest_app_from_tree(tree(&[
        ("db/structure.sql", "CREATE TABLE records (id bigint PRIMARY KEY); CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA public;"),
        ("db/migrate/001_create.rb", "class Recreate < ActiveRecord::Migration[8.0]; def change; execute 'unsupported'; end; end"),
    ])).unwrap();
    let gaps = survey::drain();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0].to_string().contains("CREATE EXTENSION"));
    assert!(app.schema.tables.contains_key(&Symbol::from("records")));
}

#[test]
fn arrays_preserve_element_types_and_unknown_types_are_not_guessed() {
    survey::activate();
    let schema = parse(
        "CREATE TYPE public.mood AS ENUM ('happy', 'sad'); CREATE TABLE samples (id bigserial PRIMARY KEY, labels text[] NOT NULL, moods public.mood[], point geometry, note text); CREATE INDEX sample_points ON samples (point);",
    );
    let gaps = survey::drain();
    assert_eq!(gaps.len(), 4, "{gaps:?}");
    let table = &schema.tables[&Symbol::from("samples")];
    assert_eq!(table.columns.len(), 4);
    assert_eq!(
        table.columns[1].col_type,
        ColumnType::Array {
            element: Box::new(ColumnType::Text)
        }
    );
    assert_eq!(
        table.columns[2].col_type,
        ColumnType::Array {
            element: Box::new(ColumnType::String { limit: None })
        }
    );
    assert!(table.indexes.is_empty());
    let model = ingest_model(
        b"class Sample < ApplicationRecord; end",
        "sample.rb",
        &schema,
        &Default::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        model.attributes.fields[&Symbol::from("labels")],
        roundhouse::ty::Ty::Array {
            elem: Box::new(roundhouse::ty::Ty::Str)
        }
    );
}

#[test]
fn namespaces_types_and_inline_constraints_are_inferred() {
    let schema = parse(
        "SET search_path = archive, public; CREATE TABLE records (key uuid PRIMARY KEY, small int2, integer int4, big int8, ratio float4, bits bit varying(8), address cidr, label public.citext, parent uuid REFERENCES archive.records(key) ON DELETE SET NULL, count integer CHECK (count > 0), name text UNIQUE);",
    );
    let table = &schema.tables[&Symbol::from("archive.records")];
    assert_eq!(table.columns[0].col_type, ColumnType::Uuid);
    assert_eq!(table.columns[1].col_type, ColumnType::Integer);
    assert_eq!(table.columns[3].col_type, ColumnType::BigInt);
    assert_eq!(table.columns[4].col_type, ColumnType::Float);
    assert_eq!(table.columns[7].col_type, ColumnType::Text);
    assert_eq!(table.foreign_keys[0].on_delete, ReferentialAction::SetNull);
    assert_eq!(table.check_constraints.len(), 1);
    assert_eq!(table.indexes.len(), 1);
    assert!(!table.columns.iter().any(|c| c.name.as_str() == "id"));
}

#[test]
fn nontrivial_index_semantics_are_not_flattened_into_plain_indexes() {
    survey::activate();
    let schema = parse(
        "CREATE TABLE records (name text, active boolean, payload jsonb); CREATE UNIQUE INDEX active_names ON records (name) WHERE active; CREATE INDEX lower_names ON records (lower(name)); CREATE INDEX payload_gin ON records USING gin (payload); CREATE INDEX name_pattern ON records (name text_pattern_ops);",
    );
    let gaps = survey::drain();
    assert_eq!(gaps.len(), 4, "{gaps:?}");
    assert!(schema.tables[&Symbol::from("records")].indexes.is_empty());
}

#[test]
fn metadata_does_not_hide_other_alter_operations() {
    let schema = parse(
        "CREATE TABLE records (id bigint); ALTER TABLE records ALTER COLUMN id SET NOT NULL, OWNER TO postgres;",
    );
    assert!(!schema.tables[&Symbol::from("records")].columns[0].nullable);
    survey::activate();
    let schema = parse(
        "CREATE TABLE records (id bigint); SELECT pg_catalog.set_config('search_path', 'public', false), meaningful_function();",
    );
    let gaps = survey::drain();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert_eq!(schema.tables.len(), 1);
}

#[test]
fn quoted_search_paths_and_numeric_restrict_keys_are_metadata() {
    let schema = parse(
        "\\restrict 12abc345\nSET search_path = \"Some,Schema\", public; CREATE TABLE records (id bigint); SELECT pg_catalog.set_config('search_path', '\"Other,Schema\", public', false); CREATE TABLE records (id integer);\\unrestrict 12abc345\n",
    );
    assert!(
        schema
            .tables
            .contains_key(&Symbol::from("Some,Schema.records"))
    );
    assert!(
        schema
            .tables
            .contains_key(&Symbol::from("Other,Schema.records"))
    );
}

#[test]
fn user_dependent_search_paths_report_gaps_in_all_setting_forms() {
    for setting in [
        r#"SET search_path = "$user", public"#,
        r#"SET search_path TO "$user", public"#,
        r#"SET search_path = '"$user", public'"#,
        r#"SELECT pg_catalog.set_config('search_path', '"$user", public', false)"#,
    ] {
        let sql = format!(
            "{setting}; CREATE TABLE records (id bigint); CREATE TABLE public.qualified (id bigint);"
        );
        let error = ingest_structure_sql(sql.as_bytes(), "db/structure.sql").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot statically resolve search_path"),
            "{setting}: {error}"
        );

        survey::activate();
        let schema = parse(&sql);
        let gaps = survey::drain();
        assert_eq!(gaps.len(), 1, "{setting}: {gaps:?}");
        assert!(
            gaps[0]
                .to_string()
                .contains("cannot statically resolve search_path")
        );
        assert!(!schema.tables.contains_key(&Symbol::from("$user.records")));
        assert!(schema.tables.contains_key(&Symbol::from("qualified")));
    }
}

#[test]
fn one_invalid_object_does_not_discard_other_schema_facts_in_survey() {
    survey::activate();
    let schema = parse(
        "CREATE TABLE before_error (id bigint); CREATE TABLE other_database.public.bad (id bigint); CREATE TABLE after_error (id bigint);",
    );
    let gaps = survey::drain();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(schema.tables.contains_key(&Symbol::from("before_error")));
    assert!(schema.tables.contains_key(&Symbol::from("after_error")));
}

#[test]
fn arbitrary_configuration_blocks_are_not_assumed_to_execute() {
    let result = ingest_app_from_tree(tree(&[
        ("db/schema.rb", "ActiveRecord::Schema.define do; end"),
        ("db/structure.sql", "CREATE TABLE records (id bigint)"),
        (
            "config/application.rb",
            "custom_loader.configure do; config.active_record.schema_format = :sql; end",
        ),
    ]));
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("cannot statically resolve")
    );
}

#[test]
fn literal_defaults_produce_the_same_constructor_as_ruby_schema() {
    let initializer = |schema: &roundhouse::schema::Schema| {
        let model = ingest_model(
            b"class Record < ApplicationRecord; end",
            "app/models/record.rb",
            schema,
            &Default::default(),
        )
        .unwrap()
        .unwrap();
        let class =
            roundhouse::lower::model_to_library::lower_model_to_library_class(&model, schema);
        roundhouse::emit::ruby::emit_method(
            class
                .methods
                .iter()
                .find(|method| method.name.as_str() == "initialize")
                .unwrap(),
        )
    };
    for (expression, expected) in [
        ("'untitled'", "untitled"),
        ("'untitled'::text", "untitled"),
        ("('untitled'::text)", "untitled"),
        ("'untitled'::character varying", "untitled"),
        ("'it''s; still one literal'", "it's; still one literal"),
        (r"E'line\nbreak'", "line\nbreak"),
        (r"U&'d\0061ta'", "data"),
        ("$label$editor's choice$label$", "editor's choice"),
        ("'draft'::public.article_state", "draft"),
    ] {
        let ruby = ingest_schema(
            format!(
                "ActiveRecord::Schema.define do; create_table :records do |t|; t.text :title, null: false, default: {}; end; end",
                serde_json::to_string(expected).unwrap(),
            ).as_bytes(),
            "db/schema.rb",
        ).unwrap();
        for ddl in [
            format!(
                "CREATE TABLE records (id bigint PRIMARY KEY, title text DEFAULT {expression} NOT NULL)"
            ),
            format!(
                "CREATE TABLE records (id bigint PRIMARY KEY, title text NOT NULL); ALTER TABLE records ALTER COLUMN title SET DEFAULT {expression}"
            ),
        ] {
            let sql = parse(&format!(
                "CREATE TYPE public.article_state AS ENUM ('draft'); {ddl}"
            ));
            let title = &sql.tables[&Symbol::from("records")].columns[1];
            assert_eq!(title.default.as_deref(), Some(expected), "{ddl}");
            assert_eq!(initializer(&sql), initializer(&ruby), "{ddl}");
        }
    }
}

#[test]
fn expression_defaults_are_evidence_not_constructor_literals() {
    let schema = parse(
        "CREATE TABLE records (
            id bigint PRIMARY KEY,
            score integer DEFAULT (-42),
            amount numeric DEFAULT 1.25,
            active boolean DEFAULT true,
            title text DEFAULT upper('hello'),
            short text DEFAULT 'hello'::varchar(2),
            optional text DEFAULT NULL,
            adjusted integer DEFAULT 3.6::integer,
            replaced text DEFAULT 'old'
        );
        ALTER TABLE records ALTER COLUMN replaced SET DEFAULT lower('NEW');",
    );
    let columns = &schema.tables[&Symbol::from("records")].columns;
    assert_eq!(columns[1].default.as_deref(), Some("-42"));
    assert_eq!(columns[2].default.as_deref(), Some("1.25"));
    assert_eq!(columns[3].default.as_deref(), Some("true"));
    assert!(columns[4..].iter().all(|column| column.default.is_none()));
    let declarations = &schema.postgresql.as_ref().unwrap().declarations;
    assert!(declarations.iter().any(|sql| sql.contains("upper")));
    assert!(declarations.iter().any(|sql| sql.contains("lower")));
    assert!(declarations.iter().any(|sql| sql.contains("varchar")));
}

#[test]
fn unique_constraints_on_unsupported_columns_do_not_emit_broken_indexes() {
    for ddl in [
        "CREATE TABLE records (id bigint PRIMARY KEY, name text UNIQUE, location geometry, CONSTRAINT location_key UNIQUE(location))",
        "CREATE TABLE records (id bigint PRIMARY KEY, name text UNIQUE, location geometry); ALTER TABLE records ADD CONSTRAINT location_key UNIQUE(name, location)",
    ] {
        survey::activate();
        let schema = parse(&format!(
            "{ddl}; CREATE INDEX location_index ON records(location); CREATE INDEX name_index ON records(name)"
        ));
        let gaps = survey::drain();
        assert_eq!(gaps.len(), 3, "{gaps:?}");
        assert!(
            gaps.iter()
                .any(|gap| gap.to_string().contains("unique constraint"))
        );
        let indexes = &schema.tables[&Symbol::from("records")].indexes;
        assert_eq!(indexes.len(), 2);
        assert!(indexes[0].unique);
        let ddl = roundhouse::emit::shared::schema_sql::render_schema_statements(&schema);
        assert_eq!(ddl.len(), 3);
        assert!(
            ddl.iter().all(|statement| !statement.contains("location")),
            "{ddl:?}"
        );
    }
}

#[test]
fn unrelated_schema_format_setters_do_not_affect_source_selection() {
    for config in [
        "SomeExporter.schema_format = :json",
        "SomeExporter.schema_format ||= :json",
        "SomeExporter.schema_format &&= :json",
        "SomeExporter.schema_format += suffix",
        "SomeExporter.schema_format, other = :json, 1",
        "config.exporter.schema_format = :json",
        "config.exporter.schema_format, other = :json, 1",
        "Rails.application.config.exporter.schema_format = :json",
        "def configure_export; SomeExporter.schema_format = :json; end",
    ] {
        let app = ingest_app_from_tree(tree(&[
            ("config/initializers/export.rb", config),
            (
                "db/schema.rb",
                "ActiveRecord::Schema.define do; create_table :records; end",
            ),
        ]))
        .unwrap();
        assert!(
            app.schema.tables.contains_key(&Symbol::from("records")),
            "{config}"
        );
    }
}

#[test]
fn parallel_schema_format_writes_report_gaps_and_fall_back_to_migrations() {
    for config in [
        "config.active_record.schema_format, other = :sql, 1",
        "other, Rails.application.config.active_record.schema_format = 1, :sql",
        "(config.active_record.schema_format, other), tail = [:sql, 1], 2",
        "config.active_record.schema_format, other = formats",
        "config.active_record.schema_format = :ruby; config.active_record.schema_format, other = :sql, 1",
    ] {
        for (path, dump) in [
            (
                "db/schema.rb",
                "ActiveRecord::Schema.define do; create_table :stale; end",
            ),
            ("db/structure.sql", "CREATE TABLE stale (id bigint)"),
        ] {
            let files = tree(&[
                ("config/application.rb", config),
                (path, dump),
                (
                    "db/migrate/001_create.rb",
                    "class CreateFresh < ActiveRecord::Migration[8.0]; def change; create_table :fresh; end; end",
                ),
            ]);
            let error = ingest_app_from_tree(files.clone()).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("cannot statically resolve schema_format"),
                "{config}: {error}"
            );

            survey::activate();
            let app = ingest_app_from_tree(files).unwrap();
            let gaps = survey::drain();
            assert_eq!(gaps.len(), 1, "{config}: {gaps:?}");
            assert!(
                gaps[0]
                    .to_string()
                    .contains("cannot statically resolve schema_format")
            );
            assert_eq!(app.schema.tables.len(), 1);
            assert!(app.schema.tables.contains_key(&Symbol::from("fresh")));
        }
    }
}

#[test]
fn rails_configuration_receivers_are_matched_structurally() {
    for receiver in [
        "config.active_record",
        "self.config.active_record",
        "Rails.application.config.active_record",
        "::Rails.application.config.active_record",
        "config\n .active_record",
    ] {
        let config = format!("{receiver}.schema_format = :sql");
        let files = |config: &str| {
            tree(&[
                ("config/application.rb", config),
                (
                    "db/schema.rb",
                    "ActiveRecord::Schema.define do; create_table :stale; end",
                ),
                (
                    "db/structure.sql",
                    "CREATE TABLE records (id bigint PRIMARY KEY)",
                ),
            ])
        };
        let app = ingest_app_from_tree(files(&config)).unwrap();
        assert!(
            app.schema.tables.contains_key(&Symbol::from("records")),
            "{config}"
        );
        for operator in ["||=", "&&=", "+="] {
            let config = format!("{receiver}.schema_format {operator} :sql");
            let error = ingest_app_from_tree(files(&config)).unwrap_err();
            assert!(
                error.to_string().contains("cannot statically resolve"),
                "{config}: {error}"
            );
        }
    }
    for config in [
        "config&.active_record.schema_format = :sql",
        "config.active_record&.schema_format = :sql",
        "Rails.application&.configure do; config.active_record.schema_format = :sql; end",
    ] {
        let error = ingest_app_from_tree(tree(&[
            ("config/application.rb", config),
            (
                "db/structure.sql",
                "CREATE TABLE records (id bigint PRIMARY KEY)",
            ),
        ]))
        .unwrap_err();
        assert!(
            error.to_string().contains("cannot statically resolve"),
            "{config}: {error}"
        );
    }
}
