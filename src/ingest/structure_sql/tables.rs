use sqlparser::ast::{
    self as sql, AlterColumnOperation, AlterTableOperation, ColumnOption, Expr, TableConstraint,
};

use crate::schema::{CheckConstraint, Column, ForeignKey, Index, ReferentialAction, Table};
use crate::{Symbol, TableRef};

use super::{Reader, ident_value};
use crate::ingest::IngestResult;

impl Reader<'_> {
    pub(super) fn create_table(&mut self, def: sql::CreateTable) -> IngestResult<()> {
        let name = self.name(&def.name)?;
        if self.schema.tables.contains_key(&name) {
            return self.gap(format!("duplicate table {name}"));
        }
        let mut table = Table {
            name: name.clone(),
            columns: vec![],
            indexes: vec![],
            foreign_keys: vec![],
            check_constraints: vec![],
            virtual_module: None,
        };
        for column in &def.columns {
            self.column(&mut table, column)?;
        }
        for constraint in &def.constraints {
            self.constraint(&mut table, constraint)?;
        }
        self.schema.tables.insert(name, table);
        if def.inherits.is_some()
            || def.partition_by.is_some()
            || def.query.is_some()
            || def.like.is_some()
        {
            self.gap(format!("table {} uses inheritance, partitioning, LIKE or AS; only explicit columns were inferred", def.name))?;
        }
        Ok(())
    }

    fn column(&self, table: &mut Table, def: &sql::ColumnDef) -> IngestResult<()> {
        let name = Symbol::from(ident_value(&def.name));
        let Some(col_type) = self.column_type(&def.data_type) else {
            return self.gap(format!(
                "column {}.{name} has unsupported type `{}`",
                table.name, def.data_type
            ));
        };
        if matches!(col_type, crate::schema::ColumnType::Array { .. }) {
            self.gap(format!(
                "array column {}.{name} inferred; PostgreSQL array persistence is not reproduced",
                table.name
            ))?;
        }
        let serial = matches!(
            def.data_type.to_string().to_lowercase().as_str(),
            "serial" | "smallserial" | "bigserial" | "serial2" | "serial4" | "serial8"
        );
        let mut column = Column {
            name: name.clone(),
            col_type,
            nullable: !serial,
            default: None,
            primary_key: false,
        };
        for option in &def.options {
            match &option.option {
                ColumnOption::Null => column.nullable = true,
                ColumnOption::NotNull => column.nullable = false,
                ColumnOption::Default(expr) => column.default = self.literal_default(expr),
                ColumnOption::Unique {
                    is_primary: true,
                    characteristics,
                } => {
                    if characteristics.is_some() {
                        self.gap(format!(
                            "primary key on {}.{name} has unsupported deferrability",
                            table.name
                        ))?;
                    }
                    column.primary_key = true;
                    column.nullable = false;
                }
                ColumnOption::Unique {
                    is_primary: false,
                    characteristics,
                } => {
                    if characteristics.is_some() {
                        self.gap(format!(
                            "unique constraint on {}.{name} has unsupported deferrability",
                            table.name
                        ))?;
                    }
                    table.indexes.push(Index {
                        name: option
                            .name
                            .as_ref()
                            .map(|n| Symbol::from(ident_value(n)))
                            .unwrap_or_else(|| {
                                Symbol::from(format!("{}_{}_key", table.name, name))
                            }),
                        columns: vec![name.clone()],
                        unique: true,
                    });
                }
                ColumnOption::Check(expr) => table.check_constraints.push(CheckConstraint {
                    name: option.name.as_ref().map(|n| Symbol::from(ident_value(n))),
                    expression: expr.to_string(),
                }),
                ColumnOption::ForeignKey {
                    foreign_table,
                    referred_columns,
                    on_delete,
                    on_update,
                    characteristics,
                } => {
                    if characteristics.is_some() {
                        self.gap(format!(
                            "foreign key on {}.{name} has unsupported deferrability",
                            table.name
                        ))?;
                    }
                    if referred_columns.len() == 1 {
                        table.foreign_keys.push(ForeignKey {
                            from_column: name.clone(),
                            to_table: TableRef(self.name(foreign_table)?),
                            to_column: Symbol::from(ident_value(&referred_columns[0])),
                            on_delete: action(*on_delete),
                            on_update: action(*on_update),
                        });
                    } else {
                        self.gap(format!(
                            "foreign key on {}.{name} requires one explicit referenced column",
                            table.name
                        ))?;
                    }
                }
                ColumnOption::Generated {
                    generation_expr: None,
                    ..
                } => column.nullable = false,
                _ => self.gap(format!(
                    "column {}.{name}: unsupported {}; database behavior is not reproduced",
                    table.name, option.option
                ))?,
            }
        }
        table.columns.push(column);
        Ok(())
    }

    fn constraint(&self, table: &mut Table, constraint: &TableConstraint) -> IngestResult<()> {
        match constraint {
            TableConstraint::PrimaryKey {
                columns,
                characteristics,
                ..
            } => {
                if characteristics.is_some() {
                    self.gap(format!(
                        "primary key on {} has unsupported deferrability",
                        table.name
                    ))?;
                }
                let Some(names) = index_columns(columns) else {
                    return self.gap(format!("unsupported primary key on {}", table.name));
                };
                for name in &names {
                    if let Some(column) = table.columns.iter_mut().find(|c| c.name == *name) {
                        column.primary_key = true;
                        column.nullable = false;
                    } else {
                        self.gap(format!(
                            "primary key on {} references missing column {name}",
                            table.name
                        ))?;
                    }
                }
                if names.len() > 1 {
                    self.gap(format!("composite primary key on {} inferred; composite key runtime behavior is not reproduced", table.name))?;
                }
            }
            TableConstraint::Unique {
                name,
                columns,
                characteristics,
                nulls_distinct,
                ..
            } => {
                if characteristics.is_some() || *nulls_distinct != sql::NullsDistinctOption::None {
                    return self.gap(format!(
                        "unsupported unique constraint semantics on {}: {constraint}",
                        table.name
                    ));
                }
                let Some(columns) = index_columns(columns) else {
                    return self.gap(format!(
                        "unsupported unique constraint on {}: {constraint}",
                        table.name
                    ));
                };
                if !has_columns(table, &columns) {
                    return self.gap(format!(
                        "unique constraint on {} references a missing or unsupported column: {constraint}",
                        table.name
                    ));
                }
                table.indexes.push(Index {
                    name: name
                        .as_ref()
                        .map(|n| Symbol::from(ident_value(n)))
                        .unwrap_or_else(|| {
                            Symbol::from(format!(
                                "{}_{}_key",
                                table.name,
                                columns
                                    .iter()
                                    .map(|c| c.as_str())
                                    .collect::<Vec<_>>()
                                    .join("_")
                            ))
                        }),
                    columns,
                    unique: true,
                });
            }
            TableConstraint::ForeignKey {
                columns,
                foreign_table,
                referred_columns,
                on_delete,
                on_update,
                characteristics,
                ..
            } => {
                if columns.len() != 1 || referred_columns.len() != 1 {
                    return self.gap(format!("composite or implicit foreign key on {} retained in PostgreSQL declarations: {constraint}", table.name));
                }
                if characteristics.is_some() {
                    self.gap(format!(
                        "foreign key on {} has unsupported deferrability",
                        table.name
                    ))?;
                }
                table.foreign_keys.push(ForeignKey {
                    from_column: Symbol::from(ident_value(&columns[0])),
                    to_table: TableRef(self.name(foreign_table)?),
                    to_column: Symbol::from(ident_value(&referred_columns[0])),
                    on_delete: action(*on_delete),
                    on_update: action(*on_update),
                });
            }
            TableConstraint::Check { name, expr, .. } => {
                table.check_constraints.push(CheckConstraint {
                    name: name.as_ref().map(|n| Symbol::from(ident_value(n))),
                    expression: expr.to_string(),
                })
            }
            _ => self.gap(format!(
                "unsupported constraint on {}: {constraint}",
                table.name
            ))?,
        }
        Ok(())
    }

    pub(super) fn alter_table(
        &mut self,
        name: &sql::ObjectName,
        operations: Vec<AlterTableOperation>,
    ) -> IngestResult<()> {
        let name = self.name(name)?;
        let Some(mut table) = self.schema.tables.get(&name).cloned() else {
            return self.gap(format!("ALTER TABLE references unknown table {name}"));
        };
        for operation in operations {
            match operation {
                AlterTableOperation::AddConstraint { constraint, .. } => {
                    self.constraint(&mut table, &constraint)?
                }
                AlterTableOperation::AlterColumn { column_name, op } => {
                    let Some(column) = table
                        .columns
                        .iter_mut()
                        .find(|c| c.name.as_str() == ident_value(&column_name))
                    else {
                        self.gap(format!(
                            "ALTER TABLE {name} references missing column {column_name}"
                        ))?;
                        continue;
                    };
                    match op {
                        AlterColumnOperation::SetNotNull
                        | AlterColumnOperation::AddGenerated { .. } => column.nullable = false,
                        AlterColumnOperation::DropNotNull => column.nullable = true,
                        AlterColumnOperation::SetDefault { value } => {
                            column.default = self.literal_default(&value)
                        }
                        AlterColumnOperation::DropDefault => column.default = None,
                        _ => self.gap(format!(
                            "unsupported ALTER COLUMN {name}.{column_name}: {op}"
                        ))?,
                    }
                }
                AlterTableOperation::OwnerTo { .. } => {}
                _ => self.gap(format!("unsupported ALTER TABLE {name}: {operation}"))?,
            }
        }
        self.schema.tables.insert(name, table);
        Ok(())
    }

    pub(super) fn create_index(&mut self, index: sql::CreateIndex) -> IngestResult<()> {
        let table_name = self.name(&index.table_name)?;
        let name = index
            .name
            .as_ref()
            .map(|n| self.name(n))
            .transpose()?
            .unwrap_or_else(|| Symbol::from(format!("{table_name}_index")));
        let columns = index_columns(&index.columns);
        if columns.is_none()
            || index.predicate.is_some()
            || index.nulls_distinct == Some(false)
            || index
                .using
                .as_ref()
                .is_some_and(|u| !u.to_string().eq_ignore_ascii_case("btree"))
        {
            return self.gap(format!("index {name} uses expressions, predicates, operator classes or PostgreSQL index semantics; retained in PostgreSQL declarations"));
        }
        let Some(table) = self.schema.tables.get_mut(&table_name) else {
            return self.gap(format!(
                "index {name} references unknown table {table_name}"
            ));
        };
        let columns = columns.unwrap();
        if !has_columns(table, &columns) {
            return self.gap(format!(
                "index {name} references a missing or unsupported column"
            ));
        }
        table.indexes.push(Index {
            name,
            columns,
            unique: index.unique,
        });
        Ok(())
    }
}

fn has_columns(table: &Table, names: &[Symbol]) -> bool {
    names
        .iter()
        .all(|name| table.columns.iter().any(|column| column.name == *name))
}

fn index_columns(columns: &[sql::IndexColumn]) -> Option<Vec<Symbol>> {
    columns
        .iter()
        .map(|c| match &c.column.expr {
            Expr::Identifier(i) if c.operator_class.is_none() => Some(Symbol::from(ident_value(i))),
            _ => None,
        })
        .collect()
}

fn action(action: Option<sql::ReferentialAction>) -> ReferentialAction {
    match action {
        None | Some(sql::ReferentialAction::NoAction) => ReferentialAction::NoAction,
        Some(sql::ReferentialAction::Restrict) => ReferentialAction::Restrict,
        Some(sql::ReferentialAction::Cascade) => ReferentialAction::Cascade,
        Some(sql::ReferentialAction::SetNull) => ReferentialAction::SetNull,
        Some(sql::ReferentialAction::SetDefault) => ReferentialAction::SetDefault,
    }
}
