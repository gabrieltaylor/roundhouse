use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer};

use super::{Reader, starts, text, word};
use crate::ingest::IngestResult;

impl Reader<'_> {
    pub(super) fn metadata(&mut self, tokens: &[TokenWithSpan]) -> IngestResult<bool> {
        if starts(tokens, &["COMMENT", "ON"]) {
            return Ok(true);
        }
        if starts(tokens, &["SET"]) {
            let variable = tokens
                .get(1)
                .map(|t| t.token.to_string().to_lowercase())
                .unwrap_or_default();
            if variable == "search_path" {
                match tokens.get(3).map(|t| &t.token) {
                    Some(Token::SingleQuotedString(value)) => self.set_namespace(value)?,
                    Some(Token::Word(_)) => self.set_namespace(&text(&tokens[3..]))?,
                    _ => self.gap("unsupported search_path setting")?,
                }
                return Ok(true);
            }
            if variable == "standard_conforming_strings"
                && !tokens.get(3).is_some_and(|t| word(&t.token, "ON"))
            {
                self.gap("standard_conforming_strings must be on for static dump parsing")?;
                return Ok(true);
            }
            return Ok(matches!(
                variable.as_str(),
                "statement_timeout"
                    | "lock_timeout"
                    | "idle_in_transaction_session_timeout"
                    | "transaction_timeout"
                    | "client_encoding"
                    | "standard_conforming_strings"
                    | "check_function_bodies"
                    | "xmloption"
                    | "client_min_messages"
                    | "row_security"
                    | "default_tablespace"
                    | "default_table_access_method"
            ));
        }
        if tokens.len() == 11
            && starts(tokens, &["SELECT", "PG_CATALOG"])
            && tokens[2].token == Token::Period
            && word(&tokens[3].token, "SET_CONFIG")
            && tokens[4].token == Token::LParen
            && tokens[6].token == Token::Comma
            && tokens[8].token == Token::Comma
            && word(&tokens[9].token, "FALSE")
            && tokens[10].token == Token::RParen
        {
            if let (Token::SingleQuotedString(setting), Token::SingleQuotedString(value)) =
                (&tokens[5].token, &tokens[7].token)
            {
                if setting == "search_path" {
                    self.set_namespace(value)?;
                    return Ok(true);
                }
            }
        }
        if starts(tokens, &["ALTER"]) {
            if ownership(tokens) {
                return Ok(true);
            }
            if starts(tokens, &["ALTER", "SEQUENCE"])
                && tokens
                    .windows(2)
                    .any(|t| word(&t[0].token, "OWNED") && word(&t[1].token, "BY"))
            {
                self.schema
                    .postgresql
                    .as_mut()
                    .unwrap()
                    .declarations
                    .push(text(tokens));
                return Ok(true);
            }
        }
        if starts(tokens, &["CREATE", "SCHEMA"]) && tokens.len() == 3 {
            return Ok(true);
        }
        if starts(tokens, &["INSERT", "INTO"]) {
            let mut parser =
                Parser::new(&PostgreSqlDialect {}).with_tokens_with_locations(tokens[2..].to_vec());
            if let Ok(name) = parser.parse_object_name(false) {
                let name = self.name(&name)?;
                if matches!(name.as_str(), "schema_migrations" | "ar_internal_metadata") {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn set_namespace(&mut self, path: &str) -> IngestResult<()> {
        if path.is_empty() {
            self.namespace = "public".into();
            return Ok(());
        }
        let tokens = Tokenizer::new(&PostgreSqlDialect {}, path).tokenize().ok();
        match tokens
            .as_ref()
            .and_then(|t| t.iter().find(|t| !matches!(t, Token::Whitespace(_))))
        {
            Some(Token::Word(w)) if w.value != "$user" => {
                self.namespace = if w.quote_style.is_some() {
                    w.value.clone()
                } else {
                    w.value.to_lowercase()
                };
                Ok(())
            }
            _ => self.gap(format!("cannot statically resolve search_path {path}")),
        }
    }
}

fn ownership(tokens: &[TokenWithSpan]) -> bool {
    if tokens.len() < 6 {
        return false;
    }
    let mut start = 2;
    if word(&tokens[2].token, "ONLY") {
        start += 1;
    }
    let mut parser =
        Parser::new(&PostgreSqlDialect {}).with_tokens_with_locations(tokens[start..].to_vec());
    if parser.parse_object_name(false).is_err() {
        return false;
    }
    let mut end = start + parser.index();
    if tokens.get(end).is_some_and(|t| t.token == Token::LParen) {
        let mut depth = 0;
        while let Some(t) = tokens.get(end) {
            match t.token {
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                _ => {}
            }
            end += 1;
            if depth == 0 {
                break;
            }
        }
    }
    tokens.len() == end + 3 && starts(&tokens[end..], &["OWNER", "TO"])
}
