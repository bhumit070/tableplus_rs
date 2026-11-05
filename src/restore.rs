//! Reopening last time's connections and tabs (including unsaved query
//! text) at launch. Kept in `session.json`; written when connections open
//! or close and when the app quits.

use serde::{Deserialize, Serialize};

use crate::db::ObjectKind;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub sessions: Vec<SessionState>,
    /// Connection id of the session in front.
    #[serde(default)]
    pub active: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub conn_id: String,
    pub tabs: Vec<TabState>,
    #[serde(default)]
    pub active_tab: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TabState {
    Table {
        schema: Option<String>,
        name: String,
        kind: ObjectKind,
    },
    Query {
        title: String,
        text: String,
    },
}

fn path() -> std::path::PathBuf {
    crate::store::config_dir().join("session.json")
}

pub fn load() -> State {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(s: &State) {
    let p = path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(s).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let s = State {
            sessions: vec![SessionState {
                conn_id: "c1".into(),
                tabs: vec![
                    TabState::Table {
                        schema: Some("public".into()),
                        name: "users".into(),
                        kind: ObjectKind::Table,
                    },
                    TabState::Query {
                        title: "Query 1".into(),
                        text: "select 1".into(),
                    },
                ],
                active_tab: 1,
            }],
            active: Some("c1".into()),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<State>(&json).unwrap(), s);
    }
}
