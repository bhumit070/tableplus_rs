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

/// The saved state, plus a message if the file was damaged (it's moved
/// aside rather than overwritten).
pub fn load() -> (State, Option<String>) {
    crate::store::read_json(&path(), "the last session")
}

pub fn save(s: &State) -> anyhow::Result<()> {
    crate::store::write_atomic(&path(), &serde_json::to_string_pretty(s)?)
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
