//! CSV import into an existing table: the header row is matched to the
//! table's columns by name (case-insensitive); each data row becomes one
//! INSERT, all applied in one transaction.

use anyhow::{Context as _, Result, bail};

use crate::db::{Change, ColumnInfo};

#[derive(Debug)]
pub struct Csv {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
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
    if std::str::from_utf8(bytes).is_err() {
        bail!("the file isn't UTF-8 text; save it as CSV UTF-8 and try again");
    }
    let first_line = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
    let delimiter = b",;\t"
        .iter()
        .copied()
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
        // UTF-16 LE with BOM (Excel "Unicode text").
        let mut le = vec![0xFF, 0xFE];
        for u in "x,y\n1,\u{e9}\n".encode_utf16() {
            le.extend(u.to_le_bytes());
        }
        assert_eq!(parse(&le).unwrap().rows, [vec!["1", "\u{e9}"]]);
        assert!(parse(b"a,b\n\xff\xfe\xfd,1\n").unwrap_err().to_string().contains("UTF-8"));
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
