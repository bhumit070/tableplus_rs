//! Context-aware autocomplete.
//!
//! This deliberately does *not* require a syntactically complete SQL
//! statement to parse (the editor buffer almost never is one while the
//! user is mid-keystroke). Instead it tokenizes just the text up to the
//! cursor and uses a small set of heuristics — "what's the nearest
//! preceding keyword", "is there a `FROM`/`JOIN` clause to resolve table
//! aliases against" — the same approach used by most lightweight SQL
//! editors. A full AST-based resolver (parsing the whole, completed
//! statement and walking it) is a natural follow-up once this proves out
//! in practice; see DEVELOPMENT.md's roadmap.

use db_core::catalog::Catalog;
use sqlparser::dialect::Dialect;
use sqlparser::keywords::Keyword as SqlKeyword;
use sqlparser::tokenizer::{Token, Tokenizer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    Keyword,
    Schema,
    Table,
    Column,
    Function,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub label: String,
    pub kind: SuggestionKind,
    /// Short human-readable context, e.g. a column's data type or a
    /// table's schema, shown greyed-out next to the suggestion.
    pub detail: Option<String>,
}

const STATEMENT_KEYWORDS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "TRUNCATE", "WITH",
    "EXPLAIN", "BEGIN", "COMMIT", "ROLLBACK", "GRANT", "REVOKE",
];

const CLAUSE_KEYWORDS: &[&str] = &[
    "WHERE", "GROUP BY", "ORDER BY", "HAVING", "LIMIT", "OFFSET", "JOIN", "LEFT JOIN",
    "RIGHT JOIN", "INNER JOIN", "FULL JOIN", "ON", "AND", "OR", "NOT", "IN", "IS NULL",
    "IS NOT NULL", "AS", "DISTINCT", "UNION", "UNION ALL",
];

const FUNCTIONS: &[&str] = &[
    "COUNT", "SUM", "AVG", "MIN", "MAX", "COALESCE", "NULLIF", "CAST", "NOW", "LOWER", "UPPER",
    "TRIM", "SUBSTRING", "CONCAT", "LENGTH", "ROUND",
];

/// Returns ranked completions for the SQL in `full_text`, as if the
/// cursor were at byte offset `cursor` (must land on a char boundary).
/// `catalog` is optional so a brand-new scratch tab with no connection
/// yet still gets keyword/function completion.
pub fn complete(full_text: &str, cursor: usize, catalog: Option<&Catalog>, dialect: &dyn Dialect) -> Vec<Suggestion> {
    let prefix_text = &full_text[..cursor.min(full_text.len())];
    let mut tokenizer = Tokenizer::new(dialect, prefix_text);
    let Ok(raw_tokens) = tokenizer.tokenize() else { return Vec::new() };

    let mut significant: Vec<Token> =
        raw_tokens.into_iter().filter(|t| !matches!(t, Token::Whitespace(_))).collect();

    let ends_mid_word = prefix_text.chars().last().is_some_and(|c| c.is_alphanumeric() || c == '_');
    let partial = if ends_mid_word {
        match significant.pop() {
            Some(Token::Word(w)) => w.value,
            Some(other) => {
                significant.push(other);
                String::new()
            }
            None => String::new(),
        }
    } else {
        String::new()
    };

    let table_refs = extract_table_refs(&significant);

    let suggestions = if matches!(significant.last(), Some(Token::Period)) {
        significant.pop();
        match significant.last() {
            Some(Token::Word(w)) => columns_for_alias(&w.value, &table_refs, catalog),
            _ => Vec::new(),
        }
    } else {
        match nearest_keyword(&significant) {
            Some(kw) if is_table_position_keyword(&kw) => table_and_schema_suggestions(catalog),
            Some(_) => column_function_and_clause_suggestions(&table_refs, catalog),
            None => statement_start_suggestions(),
        }
    };

    filter_and_rank(suggestions, &partial)
}

struct TableRef {
    /// The bare table name (schema qualifier, if any, is dropped — we
    /// only need it to resolve `alias.column`, and column names don't
    /// collide across schemas often enough to justify tracking it).
    name: String,
    alias: String,
}

fn extract_table_refs(tokens: &[Token]) -> Vec<TableRef> {
    let mut refs = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if is_keyword_token(&tokens[i], &[SqlKeyword::FROM, SqlKeyword::JOIN]) {
            i += 1;
            let Some(Token::Word(name_word)) = tokens.get(i) else {
                continue;
            };
            let mut name = name_word.value.clone();
            i += 1;

            if matches!(tokens.get(i), Some(Token::Period)) {
                i += 1;
                if let Some(Token::Word(table_part)) = tokens.get(i) {
                    name = table_part.value.clone();
                    i += 1;
                }
            }

            if is_keyword_token(tokens.get(i).unwrap_or(&Token::EOF), &[SqlKeyword::AS]) {
                i += 1;
            }

            let mut alias = name.clone();
            if let Some(Token::Word(alias_word)) = tokens.get(i) {
                if alias_word.keyword == SqlKeyword::NoKeyword {
                    alias = alias_word.value.clone();
                    i += 1;
                }
            }

            refs.push(TableRef { name, alias });
            continue;
        }
        i += 1;
    }
    refs
}

fn is_keyword_token(token: &Token, keywords: &[SqlKeyword]) -> bool {
    matches!(token, Token::Word(w) if keywords.contains(&w.keyword))
}

/// Scans backward for the nearest preceding keyword token, returning its
/// uppercased text (e.g. `"FROM"`, `"WHERE"`).
fn nearest_keyword(tokens: &[Token]) -> Option<String> {
    tokens.iter().rev().find_map(|t| match t {
        Token::Word(w) if w.keyword != SqlKeyword::NoKeyword => Some(w.value.to_uppercase()),
        _ => None,
    })
}

fn is_table_position_keyword(keyword: &str) -> bool {
    matches!(keyword, "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE")
}

fn table_and_schema_suggestions(catalog: Option<&Catalog>) -> Vec<Suggestion> {
    let Some(catalog) = catalog else { return Vec::new() };
    catalog
        .all_tables()
        .map(|(db, schema, table)| Suggestion {
            label: table.name.clone(),
            kind: SuggestionKind::Table,
            detail: Some(format!("{}.{}", db.name, schema.name)),
        })
        .collect()
}

fn columns_for_alias(alias: &str, table_refs: &[TableRef], catalog: Option<&Catalog>) -> Vec<Suggestion> {
    let Some(catalog) = catalog else { return Vec::new() };
    let Some(table_ref) = table_refs.iter().find(|r| r.alias.eq_ignore_ascii_case(alias)) else {
        return Vec::new();
    };
    let Some(table) = catalog.find_table(&table_ref.name) else { return Vec::new() };
    table
        .columns
        .iter()
        .map(|c| Suggestion { label: c.name.clone(), kind: SuggestionKind::Column, detail: Some(c.data_type.clone()) })
        .collect()
}

fn column_function_and_clause_suggestions(table_refs: &[TableRef], catalog: Option<&Catalog>) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();

    if let Some(catalog) = catalog {
        let referenced_names: Vec<&str> = table_refs.iter().map(|r| r.name.as_str()).collect();
        for (_, _, table) in catalog.all_tables() {
            if !referenced_names.is_empty() && !referenced_names.iter().any(|n| n.eq_ignore_ascii_case(&table.name)) {
                continue;
            }
            for column in &table.columns {
                suggestions.push(Suggestion {
                    label: column.name.clone(),
                    kind: SuggestionKind::Column,
                    detail: Some(format!("{}.{}", table.name, column.data_type)),
                });
            }
        }
    }

    suggestions.extend(FUNCTIONS.iter().map(|f| Suggestion {
        label: (*f).to_string(),
        kind: SuggestionKind::Function,
        detail: None,
    }));
    suggestions.extend(CLAUSE_KEYWORDS.iter().map(|k| Suggestion {
        label: (*k).to_string(),
        kind: SuggestionKind::Keyword,
        detail: None,
    }));
    suggestions
}

fn statement_start_suggestions() -> Vec<Suggestion> {
    STATEMENT_KEYWORDS
        .iter()
        .map(|k| Suggestion { label: (*k).to_string(), kind: SuggestionKind::Keyword, detail: None })
        .collect()
}

fn filter_and_rank(mut suggestions: Vec<Suggestion>, partial: &str) -> Vec<Suggestion> {
    let partial_lower = partial.to_lowercase();
    if !partial_lower.is_empty() {
        suggestions.retain(|s| s.label.to_lowercase().starts_with(&partial_lower));
    }
    suggestions.sort_by(|a, b| a.label.len().cmp(&b.label.len()).then_with(|| a.label.cmp(&b.label)));
    suggestions.dedup_by(|a, b| a.label == b.label && a.kind == b.kind);
    suggestions.truncate(50);
    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;
    use db_core::catalog::{ColumnInfo, DatabaseInfo, SchemaInfo, TableInfo, TableKind};
    use sqlparser::dialect::GenericDialect;

    fn sample_catalog() -> Catalog {
        Catalog {
            databases: vec![DatabaseInfo {
                name: "app".into(),
                schemas: vec![SchemaInfo {
                    name: "public".into(),
                    functions: Vec::new(),
                    tables: vec![TableInfo {
                        name: "customers".into(),
                        kind: TableKind::Table,
                        primary_key: vec!["id".into()],
                        foreign_keys: Vec::new(),
                        indexes: Vec::new(),
                        estimated_row_count: None,
                        columns: vec![
                            ColumnInfo {
                                name: "id".into(),
                                data_type: "int4".into(),
                                nullable: false,
                                default: None,
                                is_primary_key: true,
                                ordinal: 0,
                            },
                            ColumnInfo {
                                name: "email".into(),
                                data_type: "text".into(),
                                nullable: false,
                                default: None,
                                is_primary_key: false,
                                ordinal: 1,
                            },
                        ],
                    }],
                }],
            }],
        }
    }

    #[test]
    fn suggests_statement_keywords_at_start_of_an_empty_buffer() {
        let dialect = GenericDialect {};
        let suggestions = complete("", 0, None, &dialect);
        assert!(suggestions.iter().any(|s| s.label == "SELECT"));
    }

    #[test]
    fn suggests_tables_after_from() {
        let catalog = sample_catalog();
        let dialect = GenericDialect {};
        let sql = "SELECT * FROM cust";
        let suggestions = complete(sql, sql.len(), Some(&catalog), &dialect);
        assert!(suggestions.iter().any(|s| s.label == "customers" && s.kind == SuggestionKind::Table));
    }

    #[test]
    fn suggests_columns_of_aliased_table_after_dot() {
        let catalog = sample_catalog();
        let dialect = GenericDialect {};
        let sql = "SELECT c. FROM customers c WHERE c.";
        // Cursor right after the trailing dot.
        let suggestions = complete(sql, sql.len(), Some(&catalog), &dialect);
        assert!(suggestions.iter().any(|s| s.label == "id"));
        assert!(suggestions.iter().any(|s| s.label == "email"));
    }

    #[test]
    fn suggests_columns_from_referenced_table_in_where_clause() {
        let catalog = sample_catalog();
        let dialect = GenericDialect {};
        let sql = "SELECT * FROM customers WHERE em";
        let suggestions = complete(sql, sql.len(), Some(&catalog), &dialect);
        assert!(suggestions.iter().any(|s| s.label == "email" && s.kind == SuggestionKind::Column));
    }
}
