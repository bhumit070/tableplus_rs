//! CSV import into an existing table (design.md screen 13): the header row
//! is matched to the table's columns by name (case-insensitive), or by
//! position without a header; the mapping can be changed; each kept row
//! becomes one INSERT, all applied in one transaction. An empty field is
//! NULL (or an empty string, by choice) and a quoted empty one (`""`) is an
//! empty string, matching the export.

use std::collections::HashSet;
use std::iter::once;

use anyhow::{Context as _, Result, bail};

use crate::db::{Change, ColumnInfo};

#[derive(Debug, Clone)]
pub struct Csv {
    pub headers: Vec<String>,
    /// `None` = NULL (an empty field).
    pub rows: Vec<Vec<Option<String>>>,
    /// The separator found in the first line.
    pub delimiter: char,
    /// "UTF-8", "UTF-16 LE" or "UTF-16 BE".
    pub encoding: &'static str,
}

/// The import dialog's choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The first line holds column names; off: it's data too.
    pub header: bool,
    /// Empty fields insert NULL; off: an empty string.
    pub empty_is_null: bool,
    /// Rows that fail local validation are left out instead of stopping
    /// the import (design.md answer #20).
    pub skip_invalid: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            header: true,
            empty_is_null: true,
            skip_invalid: false,
        }
    }
}

impl Csv {
    /// Column names and data rows for `header`: without a header the first
    /// line is data and columns are "column 1", "column 2", ….
    pub fn table(&self, header: bool) -> (Vec<String>, Vec<Vec<Option<String>>>) {
        if header {
            return (self.headers.clone(), self.rows.clone());
        }
        let first: Vec<Option<String>> = self.headers.iter().map(|h| Some(h.clone())).collect();
        let rows: Vec<Vec<Option<String>>> = once(first).chain(self.rows.iter().cloned()).collect();
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        let names = (1..=width).map(|n| format!("column {n}")).collect();
        (names, rows)
    }
}

/// Reads CSV (comma, semicolon or tab separated, picked from the header
/// line). A UTF-8 byte order mark is ignored.
pub fn parse(bytes: &[u8]) -> Result<Csv> {
    // Excel's "Unicode text" export is UTF-16 with a byte order mark.
    let utf16 = |data: &[u8], le: bool| -> Result<Vec<u8>> {
        let units: Vec<u16> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                if le {
                    u16::from_le_bytes(*c)
                } else {
                    u16::from_be_bytes(*c)
                }
            })
            .collect();
        String::from_utf16(&units)
            .map(String::into_bytes)
            .context("the file says UTF-16 but isn't valid UTF-16")
    };
    let owned;
    let (bytes, encoding) = if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        owned = utf16(rest, true)?;
        (&owned[..], "UTF-16 LE")
    } else if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        owned = utf16(rest, false)?;
        (&owned[..], "UTF-16 BE")
    } else {
        (
            bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes),
            "UTF-8",
        )
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        bail!("the file isn't UTF-8 text; save it as CSV UTF-8 and try again");
    };
    let first_line = text.lines().next().unwrap_or_default();
    // `max_by_key` keeps the last of equals: a header without any
    // separator (one column) is comma separated.
    let sep = ['\t', ';', ',']
        .into_iter()
        .max_by_key(|d| first_line.matches(*d).count())
        .unwrap_or(',');
    let mut records = records(text, sep)?.into_iter();
    let headers: Vec<String> = records
        .next()
        .unwrap_or_default()
        .into_iter()
        .map(|h| h.unwrap_or_default().trim().to_string())
        .collect();
    if headers.iter().all(|h| h.is_empty()) {
        bail!("the file has no header row");
    }
    // A row of empty fields carries nothing to insert, except with one
    // column, where a blank line is how a NULL is written.
    let rows = records
        .filter(|r| headers.len() == 1 || r.iter().any(Option::is_some))
        .collect();
    Ok(Csv {
        headers,
        rows,
        delimiter: sep,
        encoding,
    })
}

/// Splits CSV text into records (RFC 4180, any line ending). Unlike the
/// `csv` crate, it keeps whether a field was quoted: an empty field is
/// `None`, `""` is `Some("")`. A file ending in a newline has no extra
/// record after it.
fn records(text: &str, sep: char) -> Result<Vec<Vec<Option<String>>>> {
    let mut out = vec![];
    let mut chars = text.chars().peekable();
    let mut line = 1;
    let ends = |c: char| c == sep || c == '\n' || c == '\r';
    while chars.peek().is_some() {
        let mut rec = vec![];
        loop {
            let mut field = String::new();
            let quoted = chars.peek() == Some(&'"');
            if quoted {
                chars.next();
                loop {
                    match chars.next() {
                        None => bail!("line {line}: a quoted value is never closed"),
                        Some('"') if chars.peek() == Some(&'"') => {
                            chars.next();
                            field.push('"');
                        }
                        Some('"') => break,
                        Some(c) => {
                            line += (c == '\n') as usize;
                            field.push(c);
                        }
                    }
                }
            }
            // Unquoted text, or anything after the closing quote (kept, as
            // spreadsheets do).
            while let Some(&c) = chars.peek() {
                if ends(c) {
                    break;
                }
                field.push(c);
                chars.next();
            }
            rec.push((quoted || !field.is_empty()).then_some(field));
            match chars.next() {
                Some(c) if c == sep => {}
                Some('\r') => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    break;
                }
                _ => break,
            }
        }
        line += 1;
        out.push(rec);
    }
    Ok(out)
}

/// The default mapping: by name with a header row (duplicates go to the
/// first CSV column only), by position without one.
pub fn default_mapping(
    headers: &[String],
    table: &[ColumnInfo],
    header: bool,
) -> Vec<Option<String>> {
    if !header {
        return (0..headers.len())
            .map(|i| table.get(i).map(|c| c.name.clone()))
            .collect();
    }
    let mut taken = HashSet::new();
    headers
        .iter()
        .map(|h| {
            table
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(h))
                .map(|c| c.name.clone())
                .filter(|name| taken.insert(name.clone()))
        })
        .collect()
}

/// Table columns the import can't leave out: NOT NULL, no default, not
/// mapped, and not an integer key the database numbers itself.
pub fn missing_required(table: &[ColumnInfo], mapping: &[Option<String>]) -> Vec<String> {
    table
        .iter()
        .filter(|c| !c.nullable && c.default.is_none())
        .filter(|c| !(c.is_pk && c.data_type.to_ascii_lowercase().contains("int")))
        .filter(|c| !mapping.iter().flatten().any(|m| *m == c.name))
        .map(|c| c.name.clone())
        .collect()
}

/// Rows (0-based) with a value that `valid(type, text)` rejects, and the
/// first column at fault.
pub fn invalid_rows(
    rows: &[Vec<Option<String>>],
    mapping: &[Option<String>],
    table: &[ColumnInfo],
    valid: impl Fn(&str, &str) -> bool,
) -> Vec<(usize, String)> {
    rows.iter()
        .enumerate()
        .filter_map(|(index, row)| {
            mapping.iter().zip(row).find_map(|(column, value)| {
                let column = table.iter().find(|c| Some(&c.name) == column.as_ref())?;
                let value = value.as_deref()?;
                (!valid(&column.data_type, value)).then(|| (index, column.name.clone()))
            })
        })
        .collect()
}

/// One insert per row kept by `keep`, with `options` applied.
pub fn changes_with(
    rows: &[Vec<Option<String>>],
    mapping: &[Option<String>],
    options: Options,
    keep: impl Fn(usize) -> bool,
) -> Vec<Change> {
    rows.iter()
        .enumerate()
        .filter(|(index, _)| keep(*index))
        .map(|(_, row)| Change::Insert {
            values: mapping
                .iter()
                .zip(row)
                .filter_map(|(column, value)| {
                    let column = column.clone()?;
                    let value = match value {
                        None if !options.empty_is_null => Some(String::new()),
                        other => other.clone(),
                    };
                    Some((column, value))
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
            enum_values: vec![],
            comment: None,
        }
    }

    #[test]
    fn parses_quotes_delimiters_and_bom() {
        let c = parse(b"\xEF\xBB\xBFName;Note\nAda;\"semi; colon\"\n\nGrace;\"multi\nline\"\n")
            .unwrap();
        assert_eq!(c.headers, ["Name", "Note"]);
        assert_eq!(
            rows(&c),
            [vec!["Ada", "semi; colon"], vec!["Grace", "multi\nline"]]
        );
        let t = parse(b"a\tb\r\n1\t2\r\n").unwrap();
        assert_eq!(rows(&t), [vec!["1", "2"]]);
        // UTF-16 LE with BOM (Excel "Unicode text").
        let mut le = vec![0xFF, 0xFE];
        for u in "x,y\n1,\u{e9}\n".encode_utf16() {
            le.extend(u.to_le_bytes());
        }
        assert_eq!(rows(&parse(&le).unwrap()), [vec!["1", "\u{e9}"]]);
        assert!(
            parse(b"a,b\n\"open,1\n")
                .unwrap_err()
                .to_string()
                .contains("never closed")
        );
        assert!(
            parse(b"a,b\n\xff\xfe\xfd,1\n")
                .unwrap_err()
                .to_string()
                .contains("UTF-8")
        );
    }

    fn rows(c: &Csv) -> Vec<Vec<&str>> {
        c.rows
            .iter()
            .map(|r| r.iter().map(|v| v.as_deref().unwrap_or("NULL")).collect())
            .collect()
    }

    #[test]
    fn null_and_empty_string_survive_an_export() {
        // As `crate::export` writes them: NULL empty, "" quoted.
        let c = parse(b"a,b\n,\"\"\n\"\",x\n").unwrap();
        assert_eq!(
            c.rows,
            [
                vec![None, Some("".into())],
                vec![Some("".into()), Some("x".into())]
            ]
        );
        // One column: a blank line is a NULL row, not skipped; a value
        // with a tab doesn't make it tab separated.
        let one = parse(b"note\nhi\n\n\"\"\na\tb\n").unwrap();
        assert_eq!(one.headers, ["note"]);
        assert_eq!(
            one.rows,
            [
                vec![Some("hi".into())],
                vec![None],
                vec![Some("".into())],
                vec![Some("a\tb".into())]
            ]
        );
    }

    #[test]
    fn maps_by_name_and_skips_unknown_and_empty() {
        let c = parse(b"NAME,extra,age\nAda,x,36\nLinus,y,\n").unwrap();
        let table = [col("id"), col("name"), col("age")];
        let (headers, rows) = c.table(true);
        let m = default_mapping(&headers, &table, true);
        assert_eq!(m, [Some("name".into()), None, Some("age".into())]);
        let ch = changes_with(&rows, &m, Options::default(), |_| true);
        let Change::Insert { values } = &ch[1] else {
            panic!()
        };
        assert_eq!(
            values,
            &vec![("name".into(), Some("Linus".into())), ("age".into(), None)]
        );
        // Two CSV columns for one table column: only the first fills it.
        let dup = parse(b"Name,name\nA,B\n").unwrap();
        assert_eq!(
            default_mapping(&dup.headers, &[col("name")], true),
            [Some("name".into()), None]
        );
    }

    #[test]
    fn options_mapping_and_validation() {
        let csv = parse(b"id,Name,age\n1,Ada,36\n2,,x\n").unwrap();
        assert_eq!((csv.delimiter, csv.encoding), (',', "UTF-8"));
        let mut age = col("age");
        age.data_type = "integer".into();
        let mut name = col("name");
        name.nullable = false;
        let mut id = col("id");
        id.data_type = "integer".into();
        id.is_pk = true;
        id.nullable = false;
        let table = vec![id, name, age];

        let (headers, rows) = csv.table(true);
        let mapping = default_mapping(&headers, &table, true);
        assert_eq!(
            mapping,
            [Some("id".into()), Some("name".into()), Some("age".into())]
        );
        assert!(missing_required(&table, &mapping).is_empty());
        // Unmapping name (NOT NULL, no default) blocks; the integer key doesn't.
        let without_name = vec![Some("id".into()), None, Some("age".into())];
        assert_eq!(missing_required(&table, &without_name), ["name"]);

        let invalid = invalid_rows(&rows, &mapping, &table, |ty, text| {
            !ty.contains("int") || text.parse::<i64>().is_ok()
        });
        assert_eq!(invalid, [(1, "age".to_string())]);

        let options = Options {
            empty_is_null: false,
            ..Options::default()
        };
        let kept = changes_with(&rows, &mapping, options, |i| i != 1);
        assert_eq!(kept.len(), 1);
        let all = changes_with(&rows, &mapping, options, |_| true);
        let Change::Insert { values } = &all[1] else {
            panic!()
        };
        assert_eq!(
            values[1],
            ("name".into(), Some(String::new())),
            "empty → ''"
        );

        // Without a header the first line is data, mapped by position.
        let (headers, rows) = csv.table(false);
        assert_eq!(headers, ["column 1", "column 2", "column 3"]);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            default_mapping(&headers, &table, false)[2],
            Some("age".into())
        );
    }
}
