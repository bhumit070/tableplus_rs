//! CREATE / ALTER TABLE and CREATE / DROP INDEX statements for the
//! structure editors, per dialect.

use anyhow::{Result, bail};

use super::{ColumnInfo, Driver, ForeignKey, QueryResult};
use crate::export::ident;

/// A column as the editor describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ColDef {
    pub name: String,
    /// Written as typed, e.g. `varchar(100)`, `numeric(10,2)`.
    pub ty: String,
    pub nullable: bool,
    /// A SQL expression (`0`, `'new'`, `now()`), as typed.
    pub default: Option<String>,
    pub pk: bool,
}

impl ColDef {
    pub fn from_info(c: &ColumnInfo) -> ColDef {
        ColDef {
            name: c.name.clone(),
            ty: c.data_type.clone(),
            nullable: c.nullable,
            default: c.default.clone(),
            pk: c.is_pk,
        }
    }
}

fn table_name(driver: Driver, schema: Option<&str>, table: &str) -> String {
    match schema.filter(|s| !s.is_empty() && driver == Driver::Postgres) {
        Some(s) => format!("{}.{}", ident(driver, s), ident(driver, table)),
        None => ident(driver, table),
    }
}

/// `name type [NOT NULL] [DEFAULT x]`.
fn column_sql(driver: Driver, c: &ColDef) -> Result<String> {
    if c.name.trim().is_empty() {
        bail!("every column needs a name");
    }
    if c.ty.trim().is_empty() {
        bail!("column {} needs a type", c.name);
    }
    let mut s = format!("{} {}", ident(driver, c.name.trim()), c.ty.trim());
    if !c.nullable {
        s.push_str(" NOT NULL");
    }
    if let Some(d) = c
        .default
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        s.push_str(&format!(" DEFAULT {d}"));
    }
    Ok(s)
}

pub fn create_table(
    driver: Driver,
    schema: Option<&str>,
    table: &str,
    cols: &[ColDef],
) -> Result<String> {
    if table.trim().is_empty() {
        bail!("the table needs a name");
    }
    if cols.is_empty() {
        bail!("add at least one column");
    }
    let mut lines = cols
        .iter()
        .map(|c| column_sql(driver, c).map(|s| format!("  {s}")))
        .collect::<Result<Vec<_>>>()?;
    let pk: Vec<String> = cols
        .iter()
        .filter(|c| c.pk)
        .map(|c| ident(driver, c.name.trim()))
        .collect();
    if !pk.is_empty() {
        lines.push(format!("  PRIMARY KEY ({})", pk.join(", ")));
    }
    Ok(format!(
        "CREATE TABLE {} (\n{}\n);",
        table_name(driver, schema, table.trim()),
        lines.join(",\n")
    ))
}

/// One edit of an existing table's columns.
#[derive(Clone, Debug)]
pub enum ColChange {
    Add(ColDef),
    Drop(String),
    Alter { from: ColDef, to: ColDef },
}

/// Statements that apply `changes`, in order. SQLite can add, drop and
/// rename columns, but changing a type, NULL-ability or default needs a
/// table rebuild, which is refused here.
pub fn alter_table(
    driver: Driver,
    schema: Option<&str>,
    table: &str,
    changes: &[ColChange],
) -> Result<Vec<String>> {
    let t = table_name(driver, schema, table);
    let q = |s: &str| ident(driver, s);
    let mut out = vec![];
    for ch in changes {
        match ch {
            ColChange::Add(c) => {
                out.push(format!(
                    "ALTER TABLE {t} ADD COLUMN {};",
                    column_sql(driver, c)?
                ));
            }
            ColChange::Drop(name) => out.push(format!("ALTER TABLE {t} DROP COLUMN {};", q(name))),
            ColChange::Alter { from, to } => {
                let to = ColDef {
                    name: to.name.trim().to_string(),
                    ty: to.ty.trim().to_string(),
                    default: to
                        .default
                        .clone()
                        .map(|d| d.trim().to_string())
                        .filter(|d| !d.is_empty()),
                    ..to.clone()
                };
                if to.name.is_empty() || to.ty.is_empty() {
                    bail!("column {} needs a name and a type", from.name);
                }
                let renamed = to.name != from.name;
                let retyped = !to.ty.eq_ignore_ascii_case(&from.ty);
                let null_changed = to.nullable != from.nullable;
                let default_changed = to.default != from.default;
                if renamed {
                    out.push(format!(
                        "ALTER TABLE {t} RENAME COLUMN {} TO {};",
                        q(&from.name),
                        q(&to.name)
                    ));
                }
                let col = q(&to.name);
                match driver {
                    Driver::Postgres => {
                        if retyped {
                            out.push(format!(
                                "ALTER TABLE {t} ALTER COLUMN {col} TYPE {} USING {col}::{};",
                                to.ty, to.ty
                            ));
                        }
                        if null_changed {
                            let verb = if to.nullable { "DROP" } else { "SET" };
                            out.push(format!(
                                "ALTER TABLE {t} ALTER COLUMN {col} {verb} NOT NULL;"
                            ));
                        }
                        if default_changed {
                            out.push(match &to.default {
                                Some(d) => {
                                    format!("ALTER TABLE {t} ALTER COLUMN {col} SET DEFAULT {d};")
                                }
                                None => format!("ALTER TABLE {t} ALTER COLUMN {col} DROP DEFAULT;"),
                            });
                        }
                    }
                    // MODIFY restates the whole column definition.
                    Driver::MySql => {
                        if retyped || null_changed || default_changed {
                            out.push(format!(
                                "ALTER TABLE {t} MODIFY COLUMN {};",
                                column_sql(driver, &to)?
                            ));
                        }
                    }
                    _ => {
                        if retyped || null_changed || default_changed {
                            bail!(
                                "SQLite can't change a column's type, NULL setting or default in place \
                                 (column {}). Create a new table and copy the rows instead.",
                                from.name
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

/// `CREATE [UNIQUE] INDEX name ON table (columns);`
pub fn create_index(
    driver: Driver,
    schema: Option<&str>,
    table: &str,
    index_name: &str,
    columns: &[String],
    unique: bool,
) -> Result<String> {
    if index_name.trim().is_empty() {
        bail!("the index needs a name");
    }
    if columns.is_empty() {
        bail!("pick at least one column");
    }
    let column_list: Vec<String> = columns.iter().map(|column| ident(driver, column)).collect();
    Ok(format!(
        "CREATE {}INDEX {} ON {} ({});",
        if unique { "UNIQUE " } else { "" },
        ident(driver, index_name.trim()),
        table_name(driver, schema, table),
        column_list.join(", ")
    ))
}

/// `DROP INDEX …`: Postgres names it in the table's schema, MySQL names
/// the table too.
pub fn drop_index(driver: Driver, schema: Option<&str>, table: &str, index_name: &str) -> String {
    match driver {
        Driver::MySql => format!(
            "DROP INDEX {} ON {};",
            ident(driver, index_name),
            ident(driver, table)
        ),
        _ => format!("DROP INDEX {};", table_name(driver, schema, index_name)),
    }
}

/// One index, however the engine lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexEntry {
    pub name: String,
    pub definition: String,
}

/// Postgres and SQLite list `name, definition`; MySQL's `SHOW INDEX` has
/// one row per indexed column, grouped here into `UNIQUE (a, b)`.
pub fn index_entries(driver: Driver, indexes: &QueryResult) -> Vec<IndexEntry> {
    let position = |name: &str| {
        indexes
            .columns
            .iter()
            .position(|column| column.eq_ignore_ascii_case(name))
    };
    if driver == Driver::MySql {
        let (Some(key), Some(column), Some(non_unique)) = (
            position("Key_name"),
            position("Column_name"),
            position("Non_unique"),
        ) else {
            return vec![];
        };
        let mut entries: Vec<(String, bool, Vec<String>)> = vec![];
        for row in &indexes.rows {
            let cell = |index: usize| row.get(index).cloned().flatten().unwrap_or_default();
            let name = cell(key);
            match entries
                .iter_mut()
                .find(|(existing, _, _)| *existing == name)
            {
                Some((_, _, columns)) => columns.push(cell(column)),
                None => entries.push((name, cell(non_unique) == "0", vec![cell(column)])),
            }
        }
        return entries
            .into_iter()
            .map(|(name, unique, columns)| IndexEntry {
                definition: format!(
                    "{}({})",
                    if unique { "UNIQUE " } else { "" },
                    columns.join(", ")
                ),
                name,
            })
            .collect();
    }
    let (Some(name), definition) = (position("name"), position("definition")) else {
        return vec![];
    };
    indexes
        .rows
        .iter()
        .map(|row| IndexEntry {
            name: row.get(name).cloned().flatten().unwrap_or_default(),
            // SQLite has no SQL for the indexes it makes itself.
            definition: definition
                .and_then(|index| row.get(index).cloned().flatten())
                .unwrap_or_else(|| "(made by the table's constraints)".into()),
        })
        .collect()
}

/// What happens to referencing rows when the referenced row goes.
pub const ON_DELETE: [&str; 4] = ["NO ACTION", "CASCADE", "SET NULL", "RESTRICT"];

/// `ALTER TABLE … ADD CONSTRAINT name FOREIGN KEY (…) REFERENCES …`.
/// SQLite can't add constraints to an existing table.
pub fn add_foreign_key(
    driver: Driver,
    schema: Option<&str>,
    table: &str,
    key: &ForeignKey,
) -> Result<String> {
    if driver == Driver::Sqlite {
        bail!("SQLite can't add a foreign key to an existing table");
    }
    if key.name.trim().is_empty() {
        bail!("the foreign key needs a name");
    }
    if key.columns.is_empty() || key.columns.len() != key.ref_columns.len() {
        bail!("pick the column, and the column it refers to");
    }
    if key.ref_table.trim().is_empty() {
        bail!("pick the table it refers to");
    }
    let list = |columns: &[String]| -> String {
        columns
            .iter()
            .map(|column| ident(driver, column.trim()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let on_delete = match key.on_delete.trim() {
        "" => "NO ACTION",
        action => action,
    };
    if !ON_DELETE.contains(&on_delete) {
        bail!("unknown ON DELETE action {on_delete}");
    }
    Ok(format!(
        "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({}) ON DELETE {on_delete};",
        table_name(driver, schema, table),
        ident(driver, key.name.trim()),
        list(&key.columns),
        table_name(
            driver,
            key.ref_schema.as_deref().or(schema),
            key.ref_table.trim()
        ),
        list(&key.ref_columns),
    ))
}

/// `ALTER TABLE … DROP CONSTRAINT` (Postgres) / `DROP FOREIGN KEY` (MySQL).
pub fn drop_foreign_key(
    driver: Driver,
    schema: Option<&str>,
    table: &str,
    key_name: &str,
) -> Result<String> {
    let verb = match driver {
        Driver::Postgres => "DROP CONSTRAINT",
        Driver::MySql => "DROP FOREIGN KEY",
        _ => bail!("SQLite can't drop a foreign key from an existing table"),
    };
    Ok(format!(
        "ALTER TABLE {} {verb} {};",
        table_name(driver, schema, table),
        ident(driver, key_name)
    ))
}

/// The statements as one script; transactional where the server rolls DDL
/// back on failure (Postgres, SQLite).
pub fn script(driver: Driver, stmts: &[String]) -> String {
    let body = stmts.join("\n");
    match driver {
        Driver::Postgres | Driver::Sqlite => format!("BEGIN;\n{body}\nCOMMIT;"),
        _ => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str, nullable: bool, default: Option<&str>, pk: bool) -> ColDef {
        ColDef {
            name: name.into(),
            ty: ty.into(),
            nullable,
            default: default.map(String::from),
            pk,
        }
    }

    #[test]
    fn create_table_per_dialect() {
        let cols = [
            col("id", "integer", false, None, true),
            col("name", "varchar(100)", true, Some("'anon'"), false),
        ];
        assert_eq!(
            create_table(Driver::Postgres, Some("app"), "people", &cols).unwrap(),
            "CREATE TABLE \"app\".\"people\" (\n  \"id\" integer NOT NULL,\n  \"name\" varchar(100) DEFAULT 'anon',\n  PRIMARY KEY (\"id\")\n);"
        );
        assert!(
            create_table(Driver::MySql, None, "people", &cols)
                .unwrap()
                .starts_with("CREATE TABLE `people` (\n  `id` integer NOT NULL")
        );
        assert!(create_table(Driver::Sqlite, None, "", &cols).is_err());
        assert!(
            create_table(
                Driver::Sqlite,
                None,
                "t",
                &[col("a", "", true, None, false)]
            )
            .is_err()
        );
    }

    #[test]
    fn groups_mysql_index_rows() {
        let result = QueryResult {
            columns: [
                "Table",
                "Non_unique",
                "Key_name",
                "Seq_in_index",
                "Column_name",
            ]
            .map(String::from)
            .to_vec(),
            rows: vec![
                ["orders", "0", "PRIMARY", "1", "id"],
                ["orders", "1", "by_status", "1", "status"],
                ["orders", "1", "by_status", "2", "created_at"],
            ]
            .into_iter()
            .map(|row| row.into_iter().map(|cell| Some(cell.to_string())).collect())
            .collect(),
            ..Default::default()
        };
        assert_eq!(
            index_entries(Driver::MySql, &result),
            [
                IndexEntry {
                    name: "PRIMARY".into(),
                    definition: "UNIQUE (id)".into()
                },
                IndexEntry {
                    name: "by_status".into(),
                    definition: "(status, created_at)".into()
                },
            ]
        );
    }

    #[test]
    fn foreign_key_statements_per_dialect() {
        let key = ForeignKey {
            name: "orders_customer_fk".into(),
            columns: vec!["customer_id".into()],
            ref_schema: None,
            ref_table: "customers".into(),
            ref_columns: vec!["id".into()],
            on_update: String::new(),
            on_delete: "CASCADE".into(),
        };
        assert_eq!(
            add_foreign_key(Driver::Postgres, Some("shop"), "orders", &key).unwrap(),
            "ALTER TABLE \"shop\".\"orders\" ADD CONSTRAINT \"orders_customer_fk\" FOREIGN KEY (\"customer_id\") REFERENCES \"shop\".\"customers\" (\"id\") ON DELETE CASCADE;"
        );
        assert!(add_foreign_key(Driver::Sqlite, None, "orders", &key).is_err());
        let unnamed = ForeignKey {
            name: String::new(),
            ..key.clone()
        };
        assert!(add_foreign_key(Driver::MySql, None, "orders", &unnamed).is_err());
        assert_eq!(
            drop_foreign_key(Driver::MySql, None, "orders", "orders_customer_fk").unwrap(),
            "ALTER TABLE `orders` DROP FOREIGN KEY `orders_customer_fk`;"
        );
        assert!(drop_foreign_key(Driver::Sqlite, None, "orders", "x").is_err());
    }

    #[test]
    fn index_statements_per_dialect() {
        let columns = ["status".to_string(), "created_at".to_string()];
        assert_eq!(
            create_index(
                Driver::Postgres,
                Some("shop"),
                "orders",
                "orders_status",
                &columns,
                true
            )
            .unwrap(),
            "CREATE UNIQUE INDEX \"orders_status\" ON \"shop\".\"orders\" (\"status\", \"created_at\");"
        );
        assert!(create_index(Driver::Sqlite, None, "t", "", &columns, false).is_err());
        assert!(create_index(Driver::Sqlite, None, "t", "i", &[], false).is_err());
        assert_eq!(
            drop_index(Driver::Postgres, Some("shop"), "orders", "orders_status"),
            "DROP INDEX \"shop\".\"orders_status\";"
        );
        assert_eq!(
            drop_index(Driver::MySql, None, "orders", "orders_status"),
            "DROP INDEX `orders_status` ON `orders`;"
        );
        assert_eq!(
            drop_index(Driver::Sqlite, None, "orders", "orders_status"),
            "DROP INDEX \"orders_status\";"
        );
    }

    #[test]
    fn alter_table_per_dialect() {
        let from = col("name", "text", true, None, false);
        let to = col("full_name", "varchar(200)", false, Some("''"), false);
        let changes = [
            ColChange::Add(col("age", "int", true, None, false)),
            ColChange::Alter {
                from: from.clone(),
                to: to.clone(),
            },
            ColChange::Drop("legacy".into()),
        ];
        assert_eq!(
            alter_table(Driver::Postgres, None, "people", &changes).unwrap(),
            [
                "ALTER TABLE \"people\" ADD COLUMN \"age\" int;",
                "ALTER TABLE \"people\" RENAME COLUMN \"name\" TO \"full_name\";",
                "ALTER TABLE \"people\" ALTER COLUMN \"full_name\" TYPE varchar(200) USING \"full_name\"::varchar(200);",
                "ALTER TABLE \"people\" ALTER COLUMN \"full_name\" SET NOT NULL;",
                "ALTER TABLE \"people\" ALTER COLUMN \"full_name\" SET DEFAULT '';",
                "ALTER TABLE \"people\" DROP COLUMN \"legacy\";",
            ]
        );
        assert_eq!(
            alter_table(Driver::MySql, None, "people", &changes[1..2]).unwrap(),
            [
                "ALTER TABLE `people` RENAME COLUMN `name` TO `full_name`;",
                "ALTER TABLE `people` MODIFY COLUMN `full_name` varchar(200) NOT NULL DEFAULT '';",
            ]
        );
        // SQLite: renames work, retyping doesn't.
        let rename = ColChange::Alter {
            from: from.clone(),
            to: ColDef {
                name: "n".into(),
                ..from.clone()
            },
        };
        assert_eq!(
            alter_table(Driver::Sqlite, None, "t", &[rename])
                .unwrap()
                .len(),
            1
        );
        assert!(alter_table(Driver::Sqlite, None, "t", &changes[1..2]).is_err());
        // Unchanged columns produce nothing.
        let same = ColChange::Alter {
            from: from.clone(),
            to: from,
        };
        assert!(
            alter_table(Driver::Postgres, None, "t", &[same])
                .unwrap()
                .is_empty()
        );
    }
}
