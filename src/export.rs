//! Turns grid data into text: "Copy as …" and "Export…" both use this.
//! Every value is text or NULL (that's how results reach the grid).

use crate::db::Driver;

pub type Row = Vec<Option<String>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csv,
    Tsv,
    Json,
    JsonLines,
    Markdown,
    Html,
    SqlInsert,
}

impl Format {
    pub const ALL: [Format; 7] = [
        Format::Csv,
        Format::Tsv,
        Format::Json,
        Format::JsonLines,
        Format::Markdown,
        Format::Html,
        Format::SqlInsert,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Format::Csv => "CSV",
            Format::Tsv => "TSV",
            Format::Json => "JSON",
            Format::JsonLines => "JSON Lines",
            Format::Markdown => "Markdown",
            Format::Html => "HTML",
            Format::SqlInsert => "SQL INSERT statements",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Tsv => "tsv",
            Format::Json => "json",
            Format::JsonLines => "jsonl",
            Format::Markdown => "md",
            Format::Html => "html",
            Format::SqlInsert => "sql",
        }
    }

    pub fn from_id(id: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.extension() == id)
    }
}

/// What SQL output needs: the dialect and the table to write into.
#[derive(Clone, Debug)]
pub struct SqlTarget {
    pub driver: Driver,
    /// Schema-qualified when known.
    pub schema: Option<String>,
    pub table: String,
}

impl SqlTarget {
    pub fn ident(&self, s: &str) -> String {
        ident(self.driver, s)
    }

    fn table_name(&self) -> String {
        match &self.schema {
            Some(s) => format!("{}.{}", self.ident(s), self.ident(&self.table)),
            None => self.ident(&self.table),
        }
    }
}

pub fn ident(driver: Driver, s: &str) -> String {
    match driver {
        Driver::MySql => format!("`{}`", s.replace('`', "``")),
        _ => format!("\"{}\"", s.replace('"', "\"\"")),
    }
}

/// A value as a SQL literal; MySQL also treats backslashes as escapes.
pub fn literal(driver: Driver, v: &Option<String>) -> String {
    match v {
        None => "NULL".into(),
        Some(s) if driver == Driver::MySql => {
            format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))
        }
        Some(s) => format!("'{}'", s.replace('\'', "''")),
    }
}

fn delimited(columns: &[String], rows: &[Row], sep: char) -> String {
    let esc = |s: &str| {
        if s.contains([sep, '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    };
    let mut out = String::new();
    let mut line = |cells: Vec<String>| {
        out.push_str(&cells.join(&sep.to_string()));
        out.push('\n');
    };
    line(columns.iter().map(|c| esc(c)).collect());
    for r in rows {
        line(r.iter().map(|v| v.as_deref().map(esc).unwrap_or_default()).collect());
    }
    out
}

fn json_object(columns: &[String], row: &Row) -> serde_json::Value {
    serde_json::Value::Object(
        columns
            .iter()
            .zip(row)
            .map(|(c, v)| {
                (
                    c.clone(),
                    v.clone()
                        .map(serde_json::Value::String)
                        .unwrap_or(serde_json::Value::Null),
                )
            })
            .collect(),
    )
}

fn markdown(columns: &[String], rows: &[Row]) -> String {
    let esc = |s: &str| s.replace('|', "\\|").replace(['\n', '\r'], " ");
    let mut out = format!(
        "| {} |\n|{}\n",
        columns.iter().map(|c| esc(c)).collect::<Vec<_>>().join(" | "),
        " --- |".repeat(columns.len())
    );
    for r in rows {
        let cells: Vec<String> = r
            .iter()
            .map(|v| v.as_deref().map(esc).unwrap_or_else(|| "NULL".into()))
            .collect();
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    out
}

fn html(columns: &[String], rows: &[Row]) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let mut out = String::from(
        "<!doctype html>\n<meta charset=\"utf-8\">\n<table>\n<thead><tr>",
    );
    for c in columns {
        out.push_str(&format!("<th>{}</th>", esc(c)));
    }
    out.push_str("</tr></thead>\n<tbody>\n");
    for r in rows {
        out.push_str("<tr>");
        for v in r {
            match v {
                Some(s) => out.push_str(&format!("<td>{}</td>", esc(s))),
                None => out.push_str("<td><i>NULL</i></td>"),
            }
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody>\n</table>\n");
    out
}

/// One `INSERT` per row.
pub fn inserts(target: &SqlTarget, columns: &[String], rows: &[Row]) -> String {
    let cols = columns
        .iter()
        .map(|c| target.ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    rows.iter()
        .map(|r| {
            let vals = r
                .iter()
                .map(|v| literal(target.driver, v))
                .collect::<Vec<_>>()
                .join(", ");
            format!("INSERT INTO {} ({cols}) VALUES ({vals});\n", target.table_name())
        })
        .collect()
}

/// One `UPDATE … WHERE <key>` per row; `key` marks the key columns. Without
/// a key every column is matched, like the grid's own edits.
pub fn updates(target: &SqlTarget, columns: &[String], key: &[bool], rows: &[Row]) -> String {
    let has_key = key.iter().any(|k| *k);
    let is_key = |i: usize| if has_key { key.get(i).copied().unwrap_or(false) } else { true };
    let cond = |c: &str, v: &Option<String>| match v {
        None => format!("{} IS NULL", target.ident(c)),
        Some(_) => format!("{} = {}", target.ident(c), literal(target.driver, v)),
    };
    rows.iter()
        .map(|r| {
            let set: Vec<String> = columns
                .iter()
                .zip(r)
                .enumerate()
                .filter(|(i, _)| !has_key || !is_key(*i))
                .map(|(_, (c, v))| format!("{} = {}", target.ident(c), literal(target.driver, v)))
                .collect();
            let wh: Vec<String> = columns
                .iter()
                .zip(r)
                .enumerate()
                .filter(|(i, _)| is_key(*i))
                .map(|(_, (c, v))| cond(c, v))
                .collect();
            format!(
                "UPDATE {} SET {} WHERE {};\n",
                target.table_name(),
                set.join(", "),
                wh.join(" AND ")
            )
        })
        .collect()
}

/// `(v1, v2, …)` for pasting after `IN`; NULLs are left out (they never match).
pub fn in_list(driver: Driver, values: &[Option<String>]) -> String {
    let mut seen = std::collections::HashSet::new();
    let vals: Vec<String> = values
        .iter()
        .filter(|v| v.is_some() && seen.insert(*v))
        .map(|v| literal(driver, v))
        .collect();
    format!("({})", vals.join(", "))
}

/// The whole result in `format`. SQL needs a target table; without one it
/// uses `target.table` as given (e.g. "query_result").
pub fn render(format: Format, columns: &[String], rows: &[Row], target: &SqlTarget) -> String {
    match format {
        Format::Csv => delimited(columns, rows, ','),
        Format::Tsv => delimited(columns, rows, '\t'),
        Format::Json => {
            let arr: Vec<_> = rows.iter().map(|r| json_object(columns, r)).collect();
            let mut s = serde_json::to_string_pretty(&arr).unwrap_or_default();
            s.push('\n');
            s
        }
        Format::JsonLines => rows
            .iter()
            .map(|r| format!("{}\n", json_object(columns, r)))
            .collect(),
        Format::Markdown => markdown(columns, rows),
        Format::Html => html(columns, rows),
        Format::SqlInsert => inserts(target, columns, rows),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<String> {
        vec!["id".into(), "name".into()]
    }

    fn rows() -> Vec<Row> {
        vec![
            vec![Some("1".into()), Some("O'Neil, \"Ada\"".into())],
            vec![Some("2".into()), None],
        ]
    }

    fn pg() -> SqlTarget {
        SqlTarget {
            driver: Driver::Postgres,
            schema: Some("public".into()),
            table: "people".into(),
        }
    }

    #[test]
    fn delimited_formats_quote_when_needed() {
        assert_eq!(
            render(Format::Csv, &cols(), &rows(), &pg()),
            "id,name\n1,\"O'Neil, \"\"Ada\"\"\"\n2,\n"
        );
        assert_eq!(
            render(Format::Tsv, &cols(), &rows(), &pg()),
            "id\tname\n1\t\"O'Neil, \"\"Ada\"\"\"\n2\t\n"
        );
    }

    #[test]
    fn json_keeps_nulls() {
        let j: serde_json::Value =
            serde_json::from_str(&render(Format::Json, &cols(), &rows(), &pg())).unwrap();
        assert_eq!(j[1]["name"], serde_json::Value::Null);
        assert_eq!(j[0]["name"], "O'Neil, \"Ada\"");
        let lines = render(Format::JsonLines, &cols(), &rows(), &pg());
        assert_eq!(lines.lines().count(), 2);
    }

    #[test]
    fn markdown_and_html_escape() {
        let md = markdown(&cols(), &[vec![Some("a|b".into()), None]]);
        assert_eq!(md, "| id | name |\n| --- | --- |\n| a\\|b | NULL |\n");
        let h = html(&cols(), &[vec![Some("<b>".into()), None]]);
        assert!(h.contains("<td>&lt;b&gt;</td><td><i>NULL</i></td>"));
    }

    #[test]
    fn sql_statements_quote_per_dialect() {
        assert_eq!(
            inserts(&pg(), &cols(), &rows()[..1]),
            "INSERT INTO \"public\".\"people\" (\"id\", \"name\") VALUES ('1', 'O''Neil, \"Ada\"');\n"
        );
        let my = SqlTarget {
            driver: Driver::MySql,
            schema: None,
            table: "people".into(),
        };
        assert_eq!(
            inserts(&my, &cols(), &[vec![Some("1".into()), Some(r"a\b".into())]]),
            "INSERT INTO `people` (`id`, `name`) VALUES ('1', 'a\\\\b');\n"
        );
        assert_eq!(
            updates(&pg(), &cols(), &[true, false], &rows()[1..]),
            "UPDATE \"public\".\"people\" SET \"name\" = NULL WHERE \"id\" = '2';\n"
        );
        // No key: match on every column, NULL with IS NULL.
        assert_eq!(
            updates(&pg(), &cols(), &[], &rows()[1..]),
            "UPDATE \"public\".\"people\" SET \"id\" = '2', \"name\" = NULL WHERE \"id\" = '2' AND \"name\" IS NULL;\n"
        );
    }

    #[test]
    fn in_list_dedupes_and_skips_nulls() {
        let v = vec![Some("a".into()), None, Some("b".into()), Some("a".into())];
        assert_eq!(in_list(Driver::Postgres, &v), "('a', 'b')");
    }
}
