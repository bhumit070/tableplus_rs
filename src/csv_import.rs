//! CSV import into an existing table: the header row is matched to the
//! table's columns by name (case-insensitive); each data row becomes one
//! INSERT, all applied in one transaction. An empty field is NULL and a
//! quoted empty one (`""`) is an empty string, matching the export.

use anyhow::{Context as _, Result, bail};

use crate::db::{Change, ColumnInfo};

#[derive(Debug)]
pub struct Csv {
    pub headers: Vec<String>,
    /// `None` = NULL (an empty field).
    pub rows: Vec<Vec<Option<String>>>,
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
            .map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) })
            .collect();
        String::from_utf16(&units)
            .map(String::into_bytes)
            .context("the file says UTF-16 but isn't valid UTF-16")
    };
    let owned;
    let bytes = if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        owned = utf16(rest, true)?;
        &owned[..]
    } else if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        owned = utf16(rest, false)?;
        &owned[..]
    } else {
        bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
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
    Ok(Csv { headers, rows })
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

/// For each CSV column, the table column it fills (by name, ignoring case).
/// Two CSV columns for one table column is an error: which one wins?
pub fn map_columns(headers: &[String], table: &[ColumnInfo]) -> Result<Vec<Option<String>>> {
    let mapping: Vec<Option<String>> = headers
        .iter()
        .map(|h| {
            table
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(h))
                .map(|c| c.name.clone())
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    if let Some(dup) = mapping.iter().flatten().find(|c| !seen.insert(*c)) {
        bail!("two CSV columns would both fill {dup}; rename or remove one");
    }
    Ok(mapping)
}

/// One insert per row. Columns missing from a short row are left out, so
/// their default applies; empty fields insert NULL.
pub fn changes(csv: &Csv, mapping: &[Option<String>]) -> Vec<Change> {
    csv.rows
        .iter()
        .map(|row| Change::Insert {
            values: mapping
                .iter()
                .zip(row)
                .filter_map(|(col, v)| {
                    let col = col.clone()?;
                    Some((col, v.clone()))
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
        assert_eq!(rows(&c), [vec!["Ada", "semi; colon"], vec!["Grace", "multi\nline"]]);
        let t = parse(b"a\tb\r\n1\t2\r\n").unwrap();
        assert_eq!(rows(&t), [vec!["1", "2"]]);
        // UTF-16 LE with BOM (Excel "Unicode text").
        let mut le = vec![0xFF, 0xFE];
        for u in "x,y\n1,\u{e9}\n".encode_utf16() {
            le.extend(u.to_le_bytes());
        }
        assert_eq!(rows(&parse(&le).unwrap()), [vec!["1", "\u{e9}"]]);
        assert!(parse(b"a,b\n\"open,1\n").unwrap_err().to_string().contains("never closed"));
        assert!(parse(b"a,b\n\xff\xfe\xfd,1\n").unwrap_err().to_string().contains("UTF-8"));
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
        assert_eq!(c.rows, [vec![None, Some("".into())], vec![Some("".into()), Some("x".into())]]);
        // One column: a blank line is a NULL row, not skipped; a value
        // with a tab doesn't make it tab separated.
        let one = parse(b"note\nhi\n\n\"\"\na\tb\n").unwrap();
        assert_eq!(one.headers, ["note"]);
        assert_eq!(
            one.rows,
            [vec![Some("hi".into())], vec![None], vec![Some("".into())], vec![Some("a\tb".into())]]
        );
    }

    #[test]
    fn maps_by_name_and_skips_unknown_and_empty() {
        let c = parse(b"NAME,extra,age\nAda,x,36\nLinus,y,\n").unwrap();
        let m = map_columns(&c.headers, &[col("id"), col("name"), col("age")]).unwrap();
        assert_eq!(m, [Some("name".into()), None, Some("age".into())]);
        let ch = changes(&c, &m);
        let Change::Insert { values } = &ch[1] else { panic!() };
        assert_eq!(
            values,
            &vec![("name".into(), Some("Linus".into())), ("age".into(), None)]
        );
        let dup = parse(b"Name,name\nA,B\n").unwrap();
        assert!(map_columns(&dup.headers, &[col("name")]).is_err());
    }
}
