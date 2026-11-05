//! Saved queries ("favorites"): named SQL kept in `saved_queries.json`,
//! either for one connection or for all of them.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub id: String,
    pub name: String,
    pub sql: String,
    /// `None` = shown on every connection.
    #[serde(default)]
    pub conn_id: Option<String>,
}

fn path() -> PathBuf {
    crate::store::config_dir().join("saved_queries.json")
}

pub fn load() -> Vec<SavedQuery> {
    load_from(&path())
}

fn load_from(p: &std::path::Path) -> Vec<SavedQuery> {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Those visible on connection `conn_id`, by name.
pub fn for_connection(conn_id: &str) -> Vec<SavedQuery> {
    let mut list: Vec<_> = load()
        .into_iter()
        .filter(|q| q.conn_id.as_deref().is_none_or(|c| c == conn_id))
        .collect();
    list.sort_by_key(|q| q.name.to_lowercase());
    list
}

/// Adds `q`, or replaces the saved query with the same id.
pub fn upsert(q: SavedQuery) -> Result<()> {
    upsert_at(&path(), q)
}

fn upsert_at(p: &std::path::Path, q: SavedQuery) -> Result<()> {
    let mut list = load_from(p);
    match list.iter_mut().find(|x| x.id == q.id) {
        Some(x) => *x = q,
        None => list.push(q),
    }
    save_to(p, &list)
}

pub fn delete(id: &str) -> Result<()> {
    let p = path();
    let mut list = load_from(&p);
    list.retain(|q| q.id != id);
    save_to(&p, &list)
}

fn save_to(p: &std::path::Path, list: &[SavedQuery]) -> Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(list)?)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_replaces_by_id_and_filters_by_connection() {
        let p = std::env::temp_dir().join(format!("tabula-saved-{}.json", uuid::Uuid::new_v4()));
        let q = |id: &str, name: &str, conn: Option<&str>| SavedQuery {
            id: id.into(),
            name: name.into(),
            sql: format!("select '{name}'"),
            conn_id: conn.map(String::from),
        };
        upsert_at(&p, q("1", "Daily", Some("a"))).unwrap();
        upsert_at(&p, q("2", "everywhere", None)).unwrap();
        upsert_at(&p, q("3", "Other", Some("b"))).unwrap();
        upsert_at(&p, q("1", "Daily v2", Some("a"))).unwrap();
        let all = load_from(&p);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].name, "Daily v2");
        let for_a: Vec<_> = all
            .iter()
            .filter(|x| x.conn_id.as_deref().is_none_or(|c| c == "a"))
            .map(|x| x.id.as_str())
            .collect();
        assert_eq!(for_a, ["1", "2"]);
        let _ = std::fs::remove_file(p);
    }
}
