//! Turns grid data into text: "Copy as …" and "Export…" both use this.
//! Every value is text or NULL (that's how results reach the grid).

use crate::db::Driver;
use serde_json::{Value, to_string_pretty};

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

    /// The name on a format tile.
    pub fn short_label(self) -> &'static str {
        match self {
            Format::SqlInsert => "SQL INSERT",
            f => f.label(),
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
}

/// Export options (design.md, screen 12). Each applies only to the formats
/// it names; the defaults match "Copy as".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// CSV, TSV and Markdown: a first line of column names.
    pub header: bool,
    /// JSON: one indented object per row, or one line per row.
    pub pretty: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            header: true,
            pretty: true,
        }
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

    /// The table as SQL writes it: `"public"."orders"`.
    pub fn qualified(&self) -> String {
        self.table_name()
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

/// One delimited line. NULL is an empty field and an empty string is `""`
/// (as in Postgres' CSV), so the two stay distinguishable.
fn delimited_line(cells: &[Option<String>], sep: char) -> String {
    let esc = |s: &str| {
        if s.is_empty() || s.contains([sep, '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    };
    let mut line = cells
        .iter()
        .map(|v| v.as_deref().map(esc).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(&sep.to_string());
    line.push('\n');
    line
}

/// JSON keys for `columns`: a repeated name (`select a.id, b.id`) gets a
/// suffix (`id_2`) instead of overwriting the first one's value.
fn json_keys(columns: &[String]) -> Vec<String> {
    // Real names first, so a suffix never takes one (`id`, `id`, `id_2`).
    let mut used: std::collections::HashSet<String> = columns.iter().cloned().collect();
    let mut first = std::collections::HashSet::new();
    columns
        .iter()
        .map(|c| {
            if first.insert(c.clone()) {
                return c.clone();
            }
            let mut n = 2;
            while used.contains(&format!("{c}_{n}")) {
                n += 1;
            }
            let key = format!("{c}_{n}");
            used.insert(key.clone());
            key
        })
        .collect()
}

fn json_object(columns: &[String], row: &Row) -> Value {
    let keys = json_keys(columns);
    Value::Object(
        keys.iter()
            .zip(row)
            .map(|(c, v)| {
                (
                    c.clone(),
                    v.clone().map(Value::String).unwrap_or(Value::Null),
                )
            })
            .collect(),
    )
}

fn md_esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ")
}

fn html_esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
            format!(
                "INSERT INTO {} ({cols}) VALUES ({vals});\n",
                target.table_name()
            )
        })
        .collect()
}

/// One `UPDATE … WHERE <key>` per row; `key` marks the key columns. Without
/// a key every column is matched, like the grid's own edits.
pub fn updates(target: &SqlTarget, columns: &[String], key: &[bool], rows: &[Row]) -> String {
    let has_key = key.iter().any(|k| *k);
    let is_key = |i: usize| {
        if has_key {
            key.get(i).copied().unwrap_or(false)
        } else {
            true
        }
    };
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
            // Every column is a key: there's nothing to set.
            if set.is_empty() {
                return String::new();
            }
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

/// Writes `format` in pieces: `header`, then `rows` for each page, then
/// `footer` — so a big table never has to sit in memory at once.
pub struct Encoder<'a> {
    format: Format,
    options: Options,
    columns: &'a [String],
    target: &'a SqlTarget,
    /// Rows written so far (JSON needs commas between objects).
    written: usize,
}

impl<'a> Encoder<'a> {
    pub fn new(format: Format, columns: &'a [String], target: &'a SqlTarget) -> Self {
        Encoder {
            format,
            options: Options::default(),
            columns,
            target,
            written: 0,
        }
    }

    pub fn options(mut self, options: Options) -> Self {
        self.options = options;
        self
    }

    pub fn header(&self) -> String {
        if !self.options.header
            && matches!(self.format, Format::Csv | Format::Tsv | Format::Markdown)
        {
            return String::new();
        }
        let names: Vec<Option<String>> = self.columns.iter().cloned().map(Some).collect();
        match self.format {
            Format::Csv => delimited_line(&names, ','),
            Format::Tsv => delimited_line(&names, '\t'),
            Format::Json => "[".into(),
            Format::Markdown => format!(
                "| {} |\n|{}\n",
                self.columns
                    .iter()
                    .map(|c| md_esc(c))
                    .collect::<Vec<_>>()
                    .join(" | "),
                " --- |".repeat(self.columns.len())
            ),
            Format::Html => {
                let mut out =
                    String::from("<!doctype html>\n<meta charset=\"utf-8\">\n<table>\n<thead><tr>");
                for c in self.columns {
                    out.push_str(&format!("<th>{}</th>", html_esc(c)));
                }
                out.push_str("</tr></thead>\n<tbody>\n");
                out
            }
            Format::JsonLines | Format::SqlInsert => String::new(),
        }
    }

    pub fn rows(&mut self, rows: &[Row]) -> String {
        let mut out = String::new();
        for r in rows {
            match self.format {
                Format::Csv => out.push_str(&delimited_line(r, ',')),
                Format::Tsv => out.push_str(&delimited_line(r, '\t')),
                Format::Json => {
                    out.push_str(if self.written == 0 { "\n  " } else { ",\n  " });
                    let obj = json_object(self.columns, r);
                    if self.options.pretty {
                        let obj = to_string_pretty(&obj).unwrap_or_default();
                        out.push_str(&obj.replace('\n', "\n  "));
                    } else {
                        out.push_str(&obj.to_string());
                    }
                }
                Format::JsonLines => {
                    out.push_str(&json_object(self.columns, r).to_string());
                    out.push('\n');
                }
                Format::Markdown => {
                    let cells: Vec<String> = r
                        .iter()
                        .map(|v| v.as_deref().map(md_esc).unwrap_or_else(|| "NULL".into()))
                        .collect();
                    out.push_str(&format!("| {} |\n", cells.join(" | ")));
                }
                Format::Html => {
                    out.push_str("<tr>");
                    for v in r {
                        match v {
                            Some(s) => out.push_str(&format!("<td>{}</td>", html_esc(s))),
                            None => out.push_str("<td><i>NULL</i></td>"),
                        }
                    }
                    out.push_str("</tr>\n");
                }
                Format::SqlInsert => {
                    out.push_str(&inserts(self.target, self.columns, std::slice::from_ref(r)))
                }
            }
            self.written += 1;
        }
        out
    }

    pub fn footer(&self) -> String {
        match self.format {
            Format::Json if self.written == 0 => "]\n".into(),
            Format::Json => "\n]\n".into(),
            Format::Html => "</tbody>\n</table>\n".into(),
            _ => String::new(),
        }
    }
}

/// The whole result in `format`. SQL needs a target table; without one it
/// uses `target.table` as given (e.g. "query_result").
pub fn render(format: Format, columns: &[String], rows: &[Row], target: &SqlTarget) -> String {
    let mut e = Encoder::new(format, columns, target);
    let mut out = e.header();
    out.push_str(&e.rows(rows));
    out.push_str(&e.footer());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, from_str};

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
        let j: Value = from_str(&render(Format::Json, &cols(), &rows(), &pg())).unwrap();
        assert_eq!(j[1]["name"], Value::Null);
        assert_eq!(j[0]["name"], "O'Neil, \"Ada\"");
        let lines = render(Format::JsonLines, &cols(), &rows(), &pg());
        assert_eq!(lines.lines().count(), 2);
    }

    #[test]
    fn options_drop_header_and_compact_json() {
        let opts = Options {
            header: false,
            pretty: false,
        };
        let run = |f| {
            let (c, r, t) = (cols(), rows(), pg());
            let mut e = Encoder::new(f, &c, &t).options(opts);
            format!("{}{}{}", e.header(), e.rows(&r), e.footer())
        };
        assert_eq!(run(Format::Csv), "1,\"O'Neil, \"\"Ada\"\"\"\n2,\n");
        assert!(!run(Format::Markdown).contains("---"));
        let json = run(Format::Json);
        assert_eq!(json.lines().count(), 4, "{json}");
        let j: Value = from_str(&json).unwrap();
        assert_eq!(j[1]["id"], "2");
    }

    #[test]
    fn csv_tells_null_from_empty() {
        let r = vec![vec![None, Some(String::new())]];
        assert_eq!(render(Format::Csv, &cols(), &r, &pg()), "id,name\n,\"\"\n");
        // And it reads back that way.
        let mut rd = csv_reader(render(Format::Csv, &cols(), &r, &pg()));
        let rec = rd.records().next().unwrap().unwrap();
        assert_eq!((&rec[0], &rec[1]), ("", ""));
        let j = render(Format::Json, &cols(), &[], &pg());
        assert_eq!(j, "[]\n");
    }

    fn csv_reader(s: String) -> csv::Reader<std::io::Cursor<Vec<u8>>> {
        csv::Reader::from_reader(std::io::Cursor::new(s.into_bytes()))
    }

    #[test]
    fn json_keeps_repeated_column_names() {
        let cols = vec!["id".to_string(), "id".to_string()];
        let j = render(
            Format::JsonLines,
            &cols,
            &[vec![Some("1".into()), Some("2".into())]],
            &pg(),
        );
        assert_eq!(j.trim(), r#"{"id":"1","id_2":"2"}"#);
        let cols: Vec<String> = ["id", "id", "id_2"].map(String::from).to_vec();
        assert_eq!(json_keys(&cols), ["id", "id_3", "id_2"]);
    }

    #[test]
    fn markdown_and_html_escape() {
        let md = render(
            Format::Markdown,
            &cols(),
            &[vec![Some("a|b".into()), None]],
            &pg(),
        );
        assert_eq!(md, "| id | name |\n| --- | --- |\n| a\\|b | NULL |\n");
        let h = render(
            Format::Html,
            &cols(),
            &[vec![Some("<b>".into()), None]],
            &pg(),
        );
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
