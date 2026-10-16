//! Export/import of the *list of saved connections* (TablePlus calls this
//! "Export/Import Connections"). This is separate from [`crate::export`],
//! which exports query/table *data*.
//!
//! Passwords are never included: they live in the OS keychain (see the
//! `app` crate's `secrets` module), keyed by [`ConnectionConfig::secret_key`].
//! Exporting only ever writes connection metadata, so a shared export file
//! can't leak credentials.

use crate::connection::ConnectionConfig;
use crate::error::{DbError, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct ConnectionSetFile {
    /// Bumped if the on-disk shape ever changes incompatibly.
    version: u32,
    connections: Vec<ConnectionConfig>,
}

const CURRENT_VERSION: u32 = 1;

/// Serializes `connections` to a pretty-printed JSON document suitable for
/// writing to a `.json` file chosen by the user.
pub fn export_connections(connections: &[ConnectionConfig]) -> Result<String> {
    let file = ConnectionSetFile { version: CURRENT_VERSION, connections: connections.to_vec() };
    serde_json::to_string_pretty(&file).map_err(|e| DbError::Io(e.to_string()))
}

/// Parses a previously exported document. The caller decides how to merge
/// the result with existing connections (e.g. by `id`, so re-importing the
/// same file is idempotent).
pub fn import_connections(json_text: &str) -> Result<Vec<ConnectionConfig>> {
    let file: ConnectionSetFile =
        serde_json::from_str(json_text).map_err(|e| DbError::Io(e.to_string()))?;
    if file.version > CURRENT_VERSION {
        return Err(DbError::Io(format!(
            "connection export file is version {}, but this build only understands up to {CURRENT_VERSION}",
            file.version
        )));
    }
    Ok(file.connections)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::DriverKind;

    #[test]
    fn export_then_import_roundtrips_connections() {
        let original = vec![ConnectionConfig::new("Local Postgres", DriverKind::Postgres)];
        let json = export_connections(&original).expect("export should succeed");
        let imported = import_connections(&json).expect("import should succeed");
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].name, "Local Postgres");
    }

    #[test]
    fn import_rejects_files_from_a_newer_version() {
        let json = r#"{"version": 999, "connections": []}"#;
        let err = import_connections(json).unwrap_err();
        assert!(matches!(err, DbError::Io(_)));
    }
}
