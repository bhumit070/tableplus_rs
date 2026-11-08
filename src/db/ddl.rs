//! CREATE / ALTER TABLE and CREATE / DROP INDEX statements for the
//! structure editors, per dialect. SQL Server's differ the most: renames go
//! through `sp_rename`, defaults are named constraints, and comments are
//! `MS_Description` extended properties.

use anyhow::{Result, bail};

use super::{ColumnInfo, Driver, ForeignKey, QueryResult};
use crate::export::ident;

/// A column as the editor describes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColDef {
    pub name: String,
    /// Written as typed, e.g. `varchar(100)`, `numeric(10,2)`.
    pub ty: String,
    pub nullable: bool,
    /// A SQL expression (`0`, `'new'`, `now()`), as typed.
    pub default: Option<String>,
    pub pk: bool,
    /// Postgres / MySQL column comment; SQLite has none.
    pub comment: Option<String>,
}

impl ColDef {
    pub fn from_info(c: &ColumnInfo) -> ColDef {
        ColDef {
            name: c.name.clone(),
            ty: c.data_type.clone(),
            nullable: c.nullable,
            default: c.default.clone(),
            pk: c.is_pk,
            comment: c.comment.clone(),
        }
    }
}

fn table_name(driver: Driver, schema: Option<&str>, table: &str) -> String {
    let schema = match driver {
        Driver::Mssql => schema.or(Some("dbo")),
        _ => schema,
    };
    match schema.filter(|s| !s.is_empty() && matches!(driver, Driver::Postgres | Driver::Mssql)) {
        Some(s) => format!("{}.{}", ident(driver, s), ident(driver, table)),
        None => ident(driver, table),
    }
}

/// A comment as a string literal; MySQL also reads backslashes as escapes.
fn comment_literal(driver: Driver, text: &str) -> String {
    let quoted = text.replace('\'', "''");
    match driver {
        Driver::MySql => format!("'{}'", quoted.replace('\\', "\\\\")),
        Driver::Mssql => format!("N'{quoted}'"),
        _ => format!("'{quoted}'"),
    }
}

/// A comment worth writing: trimmed, not empty.
fn comment_of(column: &ColDef) -> Option<&str> {
    column
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// `name type [NOT NULL] [DEFAULT x]` (and `COMMENT '…'` on MySQL).
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
    if driver == Driver::MySql
        && let Some(comment) = comment_of(c)
    {
        s.push_str(&format!(" COMMENT {}", comment_literal(driver, comment)));
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
    if driver == Driver::Mssql {
        return mssql_alter_table(schema.unwrap_or("dbo"), table, changes);
    }
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
                let comment_changed = comment_of(&to) != comment_of(from);
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
                        if comment_changed {
                            let comment = comment_of(&to)
                                .map(|text| comment_literal(driver, text))
                                .unwrap_or_else(|| "NULL".into());
                            out.push(format!("COMMENT ON COLUMN {t}.{col} IS {comment};"));
                        }
                    }
                    // MODIFY restates the whole column definition.
                    Driver::MySql => {
                        if retyped || null_changed || default_changed || comment_changed {
                            out.push(format!(
                                "ALTER TABLE {t} MODIFY COLUMN {};",
                                column_sql(driver, &to)?
                            ));
                        }
                    }
                    _ => {
                        if comment_changed {
                            bail!("SQLite columns have no comments");
                        }
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

/// A column edit after trimming; errors when it lost its name or type.
fn trimmed(from: &ColDef, to: &ColDef) -> Result<ColDef> {
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
    Ok(to)
}

/// SQL Server: `N'…'` for the system procedures' name arguments.
fn mssql_text(text: &str) -> String {
    format!("N'{}'", text.replace('\'', "''"))
}

/// Drops the column's default constraint, whatever the server named it.
fn mssql_drop_default(schema: &str, table: &str, column: &str) -> String {
    let t = table_name(Driver::Mssql, Some(schema), table);
    let object = format!("object_id({})", mssql_text(&t));
    format!(
        "DECLARE @default sysname = (SELECT name FROM sys.default_constraints \
         WHERE parent_object_id = {object} \
         AND parent_column_id = columnproperty({object}, {}, 'ColumnId'));\n\
         IF @default IS NOT NULL EXEC (N'ALTER TABLE {} DROP CONSTRAINT [' + @default + N']');",
        mssql_text(column),
        t.replace('\'', "''")
    )
}

/// Sets (or with `None` removes) a column's `MS_Description`.
fn mssql_comment(schema: &str, table: &str, column: &str, comment: Option<&str>) -> Vec<String> {
    let t = table_name(Driver::Mssql, Some(schema), table);
    let object = format!("object_id({})", mssql_text(&t));
    let place = format!(
        "N'SCHEMA', {}, N'TABLE', {}, N'COLUMN', {}",
        mssql_text(schema),
        mssql_text(table),
        mssql_text(column)
    );
    let mut out = vec![format!(
        "IF EXISTS (SELECT 1 FROM sys.extended_properties WHERE class = 1 \
         AND major_id = {object} AND minor_id = columnproperty({object}, {}, 'ColumnId') \
         AND name = N'MS_Description') \
         EXEC sp_dropextendedproperty N'MS_Description', {place};",
        mssql_text(column)
    )];
    if let Some(text) = comment {
        out.push(format!(
            "EXEC sp_addextendedproperty N'MS_Description', {}, {place};",
            mssql_text(text)
        ));
    }
    out
}

fn mssql_alter_table(schema: &str, table: &str, changes: &[ColChange]) -> Result<Vec<String>> {
    let driver = Driver::Mssql;
    let t = table_name(driver, Some(schema), table);
    let q = |s: &str| ident(driver, s);
    let mut out = vec![];
    for change in changes {
        match change {
            ColChange::Add(column) => {
                out.push(format!(
                    "ALTER TABLE {t} ADD {};",
                    column_sql(driver, column)?
                ));
                if let Some(comment) = comment_of(column) {
                    out.extend(
                        mssql_comment(schema, table, column.name.trim(), Some(comment))
                            .into_iter()
                            .skip(1),
                    );
                }
            }
            // A column with a default can't be dropped before its default.
            ColChange::Drop(name) => {
                out.push(mssql_drop_default(schema, table, name));
                out.push(format!("ALTER TABLE {t} DROP COLUMN {};", q(name)));
            }
            ColChange::Alter { from, to } => {
                let to = trimmed(from, to)?;
                if to.name != from.name {
                    out.push(format!(
                        "EXEC sp_rename {}, {}, N'COLUMN';",
                        mssql_text(&format!("{t}.{}", q(&from.name))),
                        mssql_text(&to.name)
                    ));
                }
                let col = q(&to.name);
                // ALTER COLUMN restates the type and the NULL setting.
                if !to.ty.eq_ignore_ascii_case(&from.ty) || to.nullable != from.nullable {
                    let null = if to.nullable { "NULL" } else { "NOT NULL" };
                    out.push(format!(
                        "ALTER TABLE {t} ALTER COLUMN {col} {} {null};",
                        to.ty
                    ));
                }
                if to.default != from.default {
                    out.push(mssql_drop_default(schema, table, &to.name));
                    if let Some(default) = &to.default {
                        out.push(format!(
                            "ALTER TABLE {t} ADD DEFAULT ({default}) FOR {col};"
                        ));
                    }
                }
                if comment_of(&to) != comment_of(from) {
                    out.extend(mssql_comment(schema, table, &to.name, comment_of(&to)));
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
        Driver::Mssql => format!(
            "DROP INDEX {} ON {};",
            ident(driver, index_name),
            table_name(driver, schema, table)
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
        // SQL Server has no RESTRICT; its NO ACTION refuses the delete the
        // same way.
        "RESTRICT" if driver == Driver::Mssql => "NO ACTION",
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
        Driver::Postgres | Driver::Mssql => "DROP CONSTRAINT",
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
/// back on failure (Postgres, SQLite, SQL Server). On SQL Server each
/// statement is its own `GO` batch, so their `DECLARE`s don't clash, and
/// XACT_ABORT undoes the lot when one fails.
pub fn script(driver: Driver, stmts: &[String]) -> String {
    let body = stmts.join("\n");
    match driver {
        Driver::Postgres | Driver::Sqlite => format!("BEGIN;\n{body}\nCOMMIT;"),
        Driver::Mssql => format!(
            "SET XACT_ABORT ON;\nBEGIN TRANSACTION;\nGO\n{}\nGO\nCOMMIT;",
            stmts.join("\nGO\n")
        ),
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
            comment: None,
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
    fn comments_per_dialect() {
        let from = col("total", "numeric", true, None, false);
        let to = ColDef {
            comment: Some("Order total, in the order's currency".into()),
            ..from.clone()
        };
        let change = [ColChange::Alter {
            from: from.clone(),
            to: to.clone(),
        }];
        assert_eq!(
            alter_table(Driver::Postgres, None, "orders", &change).unwrap(),
            ["COMMENT ON COLUMN \"orders\".\"total\" IS 'Order total, in the order''s currency';"]
        );
        assert_eq!(
            alter_table(Driver::MySql, None, "orders", &change).unwrap(),
            [
                "ALTER TABLE `orders` MODIFY COLUMN `total` numeric COMMENT 'Order total, in the order''s currency';"
            ]
        );
        assert!(alter_table(Driver::Sqlite, None, "orders", &change).is_err());
        // Clearing it.
        let cleared = [ColChange::Alter { from: to, to: from }];
        assert_eq!(
            alter_table(Driver::Postgres, None, "orders", &cleared).unwrap(),
            ["COMMENT ON COLUMN \"orders\".\"total\" IS NULL;"]
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

    #[test]
    fn sql_server_statements() {
        let ms = Driver::Mssql;
        let cols = [col("id", "int", false, None, true)];
        assert_eq!(
            create_table(ms, None, "people", &cols).unwrap(),
            "CREATE TABLE [dbo].[people] (\n  [id] int NOT NULL,\n  PRIMARY KEY ([id])\n);"
        );
        let from = col("name", "nvarchar(40)", true, Some("N'x'"), false);
        let to = ColDef {
            name: "full_name".into(),
            nullable: false,
            default: None,
            comment: Some("Shown on invoices".into()),
            ..from.clone()
        };
        let statements = alter_table(
            ms,
            Some("sales"),
            "people",
            &[ColChange::Alter { from, to }],
        )
        .unwrap();
        assert_eq!(
            statements[0],
            "EXEC sp_rename N'[sales].[people].[name]', N'full_name', N'COLUMN';"
        );
        assert_eq!(
            statements[1],
            "ALTER TABLE [sales].[people] ALTER COLUMN [full_name] nvarchar(40) NOT NULL;"
        );
        assert!(
            statements[2].starts_with("DECLARE @default sysname"),
            "{statements:?}"
        );
        assert!(
            statements[4]
                .starts_with("EXEC sp_addextendedproperty N'MS_Description', N'Shown on invoices'")
        );
        let script = script(ms, &statements);
        assert!(script.starts_with("SET XACT_ABORT ON;\nBEGIN TRANSACTION;\nGO\n"));
        assert_eq!(
            crate::db::split::split_mssql(&script).len(),
            statements.len() + 3
        );
        assert_eq!(
            drop_index(ms, None, "people", "by_name"),
            "DROP INDEX [by_name] ON [dbo].[people];"
        );
    }
}
