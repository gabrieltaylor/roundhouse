use sqlparser::ast::{self as sql, Statement};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer, Whitespace};

use crate::Symbol;
use crate::schema::{PostgresSchema, Schema};

use super::{IngestError, IngestResult, survey};

mod defaults;
mod metadata;
mod sequences;
mod tables;
mod types;

pub fn ingest_structure_sql(source: &[u8], file: &str) -> IngestResult<Schema> {
    let source = std::str::from_utf8(source).map_err(|err| IngestError::Parse {
        file: file.into(),
        message: format!("PostgreSQL dump must be UTF-8 text: {err}"),
    })?;
    super::sources::register(file, source);
    let tokens = Tokenizer::new(&PostgreSqlDialect {}, source)
        .tokenize_with_location()
        .map_err(|err| IngestError::Parse {
            file: file.into(),
            message: err.to_string(),
        })?;
    let mut reader = Reader {
        schema: Schema {
            postgresql: Some(PostgresSchema::default()),
            ..Schema::default()
        },
        file,
        line: 1,
        namespace: "public".into(),
    };
    let mut statement = Vec::new();
    let mut meta = false;
    for token in tokens {
        if statement.is_empty() && token.token == Token::Backslash {
            meta = true;
        }
        if meta && matches!(token.token, Token::Whitespace(Whitespace::Newline)) {
            survey::unwrap_or_record(reader.meta(&statement))?;
            statement.clear();
            meta = false;
        } else if !meta && token.token == Token::SemiColon {
            survey::unwrap_or_record(reader.statement(&statement))?;
            statement.clear();
        } else if !matches!(token.token, Token::Whitespace(_)) {
            statement.push(token);
        }
    }
    if meta {
        survey::unwrap_or_record(reader.meta(&statement))?;
    } else {
        survey::unwrap_or_record(reader.statement(&statement))?;
    }
    Ok(reader.schema)
}

struct Reader<'a> {
    schema: Schema,
    file: &'a str,
    line: u64,
    namespace: String,
}

impl Reader<'_> {
    fn gap(&self, message: impl std::fmt::Display) -> IngestResult<()> {
        survey::unwrap_or_record::<()>(Err(IngestError::Unsupported {
            file: self.file.into(),
            message: format!("PostgreSQL schema: {message} (line {})", self.line),
        }))
        .map(|_| ())
    }

    fn meta(&mut self, tokens: &[TokenWithSpan]) -> IngestResult<()> {
        if tokens.is_empty() {
            return Ok(());
        }
        self.line = tokens[0].span.start.line;
        if tokens.len() >= 3
            && matches!(&tokens[1].token, Token::Word(w) if matches!(w.value.as_str(), "restrict" | "unrestrict"))
        {
            return Ok(());
        }
        self.gap(format!("unsupported psql command: {}", text(tokens)))
    }

    fn statement(&mut self, tokens: &[TokenWithSpan]) -> IngestResult<()> {
        if tokens.is_empty() {
            return Ok(());
        }
        self.line = tokens[0].span.start.line;
        if self.metadata(tokens)? {
            return Ok(());
        }
        let definition = text(tokens);
        self.schema
            .postgresql
            .as_mut()
            .unwrap()
            .declarations
            .push(definition.clone());
        let parsed = Parser::new(&PostgreSqlDialect {})
            .with_tokens_with_locations(sequences::normalize(tokens))
            .parse_statements();
        let statement = match parsed {
            Ok(mut statements) if statements.len() == 1 => statements.remove(0),
            Ok(_) => {
                return self.gap(format!(
                    "expected one DDL statement: {}",
                    preview(&definition)
                ));
            }
            Err(err) => {
                return self.gap(format!(
                    "unsupported or malformed DDL {}: {err}",
                    preview(&definition)
                ));
            }
        };
        match statement {
            Statement::CreateTable(table) => self.create_table(table),
            Statement::AlterTable {
                name, operations, ..
            } => self.alter_table(&name, operations),
            Statement::CreateIndex(index) => self.create_index(index),
            Statement::CreateType {
                name,
                representation: sql::UserDefinedTypeRepresentation::Enum { labels },
            } => {
                let name = self.name(&name)?;
                self.schema
                    .postgresql
                    .as_mut()
                    .unwrap()
                    .enums
                    .insert(name, labels.into_iter().map(|l| l.value).collect());
                Ok(())
            }
            Statement::CreateSequence { .. } => Ok(()),
            _ => self.gap(format!(
                "unsupported DDL {}; database behavior is not reproduced",
                preview(&definition)
            )),
        }
    }

    fn name(&self, name: &sql::ObjectName) -> IngestResult<Symbol> {
        let parts: Vec<_> = name
            .0
            .iter()
            .filter_map(|p| p.as_ident())
            .map(ident_value)
            .collect();
        let value = match parts.as_slice() {
            [name] if self.namespace == "public" => name.clone(),
            [name] => format!("{}.{name}", self.namespace),
            [schema, name] if schema == "public" || schema == "pg_catalog" => name.clone(),
            [schema, name] => format!("{schema}.{name}"),
            _ => {
                return Err(IngestError::Unsupported {
                    file: self.file.into(),
                    message: format!("unsupported PostgreSQL qualified name {name}"),
                });
            }
        };
        Ok(Symbol::from(value))
    }
}

fn ident_value(ident: &sql::Ident) -> String {
    if ident.quote_style.is_some() {
        ident.value.clone()
    } else {
        ident.value.to_lowercase()
    }
}

fn word(token: &Token, expected: &str) -> bool {
    matches!(token, Token::Word(w) if w.quote_style.is_none() && w.value.eq_ignore_ascii_case(expected))
}

fn starts(tokens: &[TokenWithSpan], words: &[&str]) -> bool {
    tokens.len() >= words.len() && tokens.iter().zip(words).all(|(t, w)| word(&t.token, w))
}

fn text(tokens: &[TokenWithSpan]) -> String {
    tokens
        .iter()
        .map(|t| t.token.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn preview(sql: &str) -> String {
    sql.chars().take(160).collect()
}
