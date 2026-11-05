//! CSV import into an existing table: the header row is matched to the
//! table's columns by name (case-insensitive); each data row becomes one
//! INSERT, all applied in one transaction.

use anyhow::{Context as _, Result, bail};

use crate::db::{Change, ColumnInfo};

pub struct Csv {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// Reads CSV (comma, semicolon or tab separated, picked from the header
/// line). A UTF-8 byte order mark is ignored.
pub fn parse(bytes: &[u8]) -> Result<Csv> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let first_line = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
    let delimiter = [b',', b';', b'\t']
        .into_iter()
        .max_by_key(|d| first_line.iter().filter(|b| *b == d).count())
        .unwrap_or(b',');
    let mut r = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .flexible(true)
        .from_reader(bytes);
    let headers: Vec<String> = r
        .headers()
        .context("reading the header row")?
        .iter()
        .map(|h| h.trim().to_string())
        .collect();
    if headers.iter().all(|h| h.is_empty()) {
        bail!("the file has no header row");
    }
    let mut rows = vec![];
    for (i, rec) in r.records().enumerate() {
        let rec = rec.with_context(|| format!("line {}", i + 2))?;
        // Blank lines carry nothing to insert.
        if rec.iter().all(|f| f.is_empty()) {
            continue;
        }
        rows.push(rec.iter().map(String::from).collect());
    }
    Ok(Csv { headers, rows })
}

/// For each CSV column, the table column it fills (by name, ignoring case).
pub fn map_columns(headers: &[String], table: &[ColumnInfo]) -> Vec<Option<String>> {
    headers
        .iter()
        .map(|h| {
            table
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(h))
                .map(|c| c.name.clone())
        })
        .collect()
}

/// One insert per row. Empty cells are left out, so the column's default
/// (or NULL) applies.
pub fn changes(csv: &Csv, mapping: &[Option<String>]) -> Vec<Change> {
    csv.rows
        .iter()
        .map(|row| Change::Insert {
            values: mapping
                .iter()
                .zip(row)
                .filter_map(|(col, v)| {
                    let col = col.clone()?;
                    Some((col, (!v.is_empty()).then(|| v.clone())))
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "text".into(),
            nullable: true,
            default: None,
            is_pk: false,
        }
    }

    #[test]
    fn parses_quotes_delimiters_and_bom() {
        let c = parse(b"\xEF\xBB\xBFName;Note\nAda;\"semi; colon\"\n\nGrace;\"multi\nline\"\n").unwrap();
        assert_eq!(c.headers, ["Name", "Note"]);
        assert_eq!(c.rows, [vec!["Ada", "semi; colon"], vec!["Grace", "multi\nline"]]);
        let t = parse(b"a\tb\n1\t2\n").unwrap();
        assert_eq!(t.rows, [vec!["1", "2"]]);
    }

    #[test]
    fn maps_by_name_and_skips_unknown_and_empty() {
        let c = parse(b"NAME,extra,age\nAda,x,36\nLinus,y,\n").unwrap();
        let m = map_columns(&c.headers, &[col("id"), col("name"), col("age")]);
        assert_eq!(m, [Some("name".into()), None, Some("age".into())]);
        let ch = changes(&c, &m);
        let Change::Insert { values } = &ch[1] else { panic!() };
        assert_eq!(
            values,
            &vec![("name".into(), Some("Linus".into())), ("age".into(), None)]
        );
    }
}
