//! Persists the saved connection list to a JSON file under the OS's
//! standard config directory, and implements the "Export/Import
//! Connections" feature on top of [`db_core::connection_set`].

use db_core::connection::ConnectionConfig;
use std::path::PathBuf;

fn config_dir() -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("rs", "tableplus-rs", "tableplus-rs")
        .ok_or_else(|| anyhow::anyhow!("could not determine the OS config directory"))?;
    let dir = dirs.config_dir().to_path_buf();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn connections_file() -> anyhow::Result<PathBuf> {
    Ok(config_dir()?.join("connections.json"))
}

pub fn load() -> anyhow::Result<Vec<ConnectionConfig>> {
    let path = connections_file()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    Ok(db_core::connection_set::import_connections(&text)?)
}

pub fn save(connections: &[ConnectionConfig]) -> anyhow::Result<()> {
    let json = db_core::connection_set::export_connections(connections)?;
    std::fs::write(connections_file()?, json)?;
    Ok(())
}

/// Exports the connection list (no passwords) to an arbitrary path chosen
/// by the user via a file-save dialog.
pub fn export_to(connections: &[ConnectionConfig], path: &std::path::Path) -> anyhow::Result<()> {
    let json = db_core::connection_set::export_connections(connections)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Imports a previously exported connection list from an arbitrary path
/// chosen by the user via a file-open dialog.
pub fn import_from(path: &std::path::Path) -> anyhow::Result<Vec<ConnectionConfig>> {
    let text = std::fs::read_to_string(path)?;
    Ok(db_core::connection_set::import_connections(&text)?)
}
