use sqlparser::ast::{DataType, Expr, UnaryOperator, Value};

use super::Reader;

impl Reader<'_> {
    pub(super) fn literal_default(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Value(value) => match &value.value {
                Value::Number(value, _) => Some(value.clone()),
                Value::Boolean(value) => Some(value.to_string()),
                _ => self.string_default(expr),
            },
            Expr::Nested(expr) => self.literal_default(expr),
            Expr::UnaryOp { op, expr } => {
                let Expr::Value(value) = expr.as_ref() else {
                    return None;
                };
                let Value::Number(value, _) = &value.value else {
                    return None;
                };
                match op {
                    UnaryOperator::Plus => Some(value.clone()),
                    UnaryOperator::Minus => Some(format!("-{value}")),
                    _ => None,
                }
            }
            _ => self.string_default(expr),
        }
    }

    fn string_default(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Value(value) => match &value.value {
                Value::SingleQuotedString(value)
                | Value::EscapedStringLiteral(value)
                | Value::UnicodeStringLiteral(value) => Some(value.clone()),
                Value::DollarQuotedString(value) => Some(value.value.clone()),
                _ => None,
            },
            Expr::Nested(expr) => self.string_default(expr),
            Expr::Cast {
                expr,
                data_type,
                format: None,
                ..
            } if self.preserves_string_literal(data_type) => self.string_default(expr),
            _ => None,
        }
    }

    fn preserves_string_literal(&self, ty: &DataType) -> bool {
        match ty {
            DataType::Text
            | DataType::CharacterVarying(None)
            | DataType::CharVarying(None)
            | DataType::Varchar(None) => true,
            DataType::Custom(name, modifiers) if modifiers.is_empty() => {
                self.name(name).ok().is_some_and(|name| {
                    self.schema
                        .postgresql
                        .as_ref()
                        .unwrap()
                        .enums
                        .contains_key(&name)
                })
            }
            _ => false,
        }
    }
}
