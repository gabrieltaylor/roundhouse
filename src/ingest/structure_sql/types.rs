use sqlparser::ast::{ArrayElemTypeDef, CharacterLength, DataType, ExactNumberInfo};

use crate::schema::ColumnType;

use super::Reader;

impl Reader<'_> {
    pub(super) fn column_type(&self, ty: &DataType) -> Option<ColumnType> {
        let spelling = match ty {
            DataType::Custom(name, _) if name.0.len() == 1 => {
                super::ident_value(name.0[0].as_ident()?)
            }
            DataType::Custom(name, _) => self.name(name).ok()?.as_str().to_string(),
            _ => ty.to_string().to_lowercase(),
        };
        let base = spelling.split('(').next().unwrap().trim();
        Some(match ty {
            DataType::Array(ArrayElemTypeDef::SquareBracket(element, _)) => ColumnType::Array {
                element: Box::new(self.column_type(element)?),
            },
            DataType::Numeric(n) | DataType::Decimal(n) | DataType::Dec(n) => {
                let (precision, scale) = match n {
                    ExactNumberInfo::None => (None, None),
                    ExactNumberInfo::Precision(p) => (Some((*p).try_into().ok()?), None),
                    ExactNumberInfo::PrecisionAndScale(p, s) => {
                        (Some((*p).try_into().ok()?), Some((*s).try_into().ok()?))
                    }
                };
                ColumnType::Decimal { precision, scale }
            }
            DataType::Character(n)
            | DataType::Char(n)
            | DataType::CharacterVarying(n)
            | DataType::CharVarying(n)
            | DataType::Varchar(n) => {
                let limit = match n {
                    Some(CharacterLength::IntegerLength { length, .. }) => {
                        Some((*length).try_into().ok()?)
                    }
                    None => None,
                    _ => return None,
                };
                ColumnType::String { limit }
            }
            DataType::Timestamp(..) => ColumnType::DateTime,
            DataType::Time(..) => ColumnType::Time,
            DataType::Custom(name, _)
                if self
                    .schema
                    .postgresql
                    .as_ref()?
                    .enums
                    .contains_key(&self.name(name).ok()?) =>
            {
                ColumnType::String { limit: None }
            }
            _ => match base {
                "smallint" | "integer" | "int" | "int2" | "int4" | "serial" | "smallserial"
                | "serial2" | "serial4" => ColumnType::Integer,
                "bigint" | "int8" | "bigserial" | "serial8" => ColumnType::BigInt,
                "real" | "double precision" | "float" | "float4" | "float8" => ColumnType::Float,
                "text" | "citext" => ColumnType::Text,
                "boolean" | "bool" => ColumnType::Boolean,
                "date" => ColumnType::Date,
                "timestamptz" => ColumnType::DateTime,
                "timetz" => ColumnType::Time,
                "bytea" => ColumnType::Binary,
                "json" | "jsonb" => ColumnType::Json,
                "uuid" => ColumnType::Uuid,
                "inet" | "cidr" | "macaddr" | "macaddr8" | "name" | "xml" | "bit"
                | "bit varying" | "varbit" => ColumnType::String { limit: None },
                _ => return None,
            },
        })
    }
}
