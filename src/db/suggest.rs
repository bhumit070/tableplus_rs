//! "Did you mean total?" for a query error (design.md screen 22): finds the
//! unknown column or table an error names, the closest known name, and
//! rewrites the statement with it.

/// What an error says doesn't exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unknown {
    Column(String),
    Table(String),
}

impl Unknown {
    pub fn name(&self) -> &str {
        match self {
            Unknown::Column(name) | Unknown::Table(name) => name,
        }
    }
}

/// The unknown name in a Postgres, MySQL or SQLite error message.
pub fn unknown_name(error: &str) -> Option<Unknown> {
    // `column "nope" does not exist`, `relation "x" does not exist`
    let quoted = |after: &str, open: char, close: char| -> Option<String> {
        let start = error.find(after)? + after.len();
        let rest = error[start..].trim_start().strip_prefix(open)?;
        let end = rest.find(close)?;
        Some(rest[..end].to_string())
    };
    // `no such column: nope` (SQLite), up to whitespace / end.
    let bare = |after: &str| -> Option<String> {
        let start = error.find(after)? + after.len();
        let word: String = error[start..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        (!word.is_empty()).then_some(word)
    };
    // Qualified names (`o.nope`, `shop.ordrs`): the last part.
    let last = |name: String| name.rsplit('.').next().unwrap_or(&name).to_string();
    if let Some(name) = quoted("column", '"', '"').filter(|_| error.contains("does not exist")) {
        return Some(Unknown::Column(last(name)));
    }
    if let Some(name) = quoted("relation", '"', '"').filter(|_| error.contains("does not exist")) {
        return Some(Unknown::Table(last(name)));
    }
    if let Some(name) = quoted("Unknown column", '\'', '\'') {
        return Some(Unknown::Column(last(name)));
    }
    if let Some(name) = quoted("Table", '\'', '\'').filter(|_| error.contains("doesn't exist")) {
        return Some(Unknown::Table(last(name)));
    }
    if let Some(name) = bare("no such column:") {
        return Some(Unknown::Column(last(name)));
    }
    if let Some(name) = bare("no such table:") {
        return Some(Unknown::Table(last(name)));
    }
    None
}

/// Edit distance (insertions, deletions, substitutions), case-insensitive.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(ca != cb);
            current.push(substitute.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

/// The candidate closest to `name`, when it's close enough to be a typo:
/// at most a third of the name's length away (and at least one edit).
pub fn closest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a String>) -> Option<String> {
    let allowed = (name.chars().count() / 3).max(1);
    candidates
        .into_iter()
        .filter(|candidate| !candidate.eq_ignore_ascii_case(name))
        .map(|candidate| (distance(name, candidate), candidate))
        .filter(|(edits, _)| *edits <= allowed)
        .min_by_key(|(edits, _)| *edits)
        .map(|(_, candidate)| candidate.clone())
}

/// `text` with the first whole-word `word` at or after byte `from` replaced
/// by `with` (case-insensitive); `None` when it isn't there.
pub fn replace_word(text: &str, from: usize, word: &str, with: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let target = word.to_lowercase();
    // Lowercasing can change lengths outside ASCII; then byte offsets
    // wouldn't line up, so don't guess.
    if lower.len() != text.len() {
        return None;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut at = from.min(text.len());
    while let Some(found) = lower[at..].find(&target) {
        let start = at + found;
        let end = start + target.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
            return Some(format!("{}{with}{}", &text[..start], &text[end..]));
        }
        at = end;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_engines_error() {
        assert_eq!(
            unknown_name("error returned from database: column \"totl\" does not exist"),
            Some(Unknown::Column("totl".into()))
        );
        assert_eq!(
            unknown_name("column o.totl does not exist"),
            None,
            "not quoted: not Postgres' wording"
        );
        assert_eq!(
            unknown_name("relation \"public.ordrs\" does not exist"),
            Some(Unknown::Table("ordrs".into()))
        );
        assert_eq!(
            unknown_name("Unknown column 'o.totl' in 'field list'"),
            Some(Unknown::Column("totl".into()))
        );
        assert_eq!(
            unknown_name("Table 'shop.ordrs' doesn't exist"),
            Some(Unknown::Table("ordrs".into()))
        );
        assert_eq!(
            unknown_name("(code: 1) no such column: totl"),
            Some(Unknown::Column("totl".into()))
        );
        assert_eq!(unknown_name("syntax error at or near \"form\""), None);
    }

    #[test]
    fn suggests_only_close_names() {
        let columns: Vec<String> = ["total", "status", "currency"].map(String::from).to_vec();
        assert_eq!(closest("totl", &columns).as_deref(), Some("total"));
        assert_eq!(closest("STATSU", &columns).as_deref(), Some("status"));
        assert_eq!(closest("nope", &columns), None);
    }

    #[test]
    fn replaces_whole_words_after_a_point() {
        let sql = "select totl, totla from t where totl > 1";
        assert_eq!(
            replace_word(sql, 0, "totl", "total").as_deref(),
            Some("select total, totla from t where totl > 1")
        );
        assert_eq!(
            replace_word(sql, 12, "TOTL", "total").as_deref(),
            Some("select totl, totla from t where total > 1")
        );
        assert_eq!(replace_word(sql, 0, "nope", "x"), None);
    }
}
