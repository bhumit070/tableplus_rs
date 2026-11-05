//! Saved connections: settings in `connections.json`, passwords in the OS
//! keychain (service "tabula", account = connection id).

use std::path::PathBuf;

use anyhow::Result;

use crate::db::ConnectionConfig;

const SERVICE: &str = "tabula";

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tabula")
        .join("connections.json")
}

pub fn load() -> Vec<ConnectionConfig> {
    let Ok(text) = std::fs::read_to_string(path()) else {
        return vec![];
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Passwords are read lazily, only when a connection is opened.
pub fn password(id: &str) -> String {
    keyring::Entry::new(SERVICE, id)
        .and_then(|e| e.get_password())
        .unwrap_or_default()
}

pub fn save_all(list: &[ConnectionConfig]) -> Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(p, serde_json::to_string_pretty(list)?)?;
    Ok(())
}

pub fn set_password(id: &str, password: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, id)?;
    if password.is_empty() {
        let _ = entry.delete_credential();
    } else {
        entry.set_password(password)?;
    }
    Ok(())
}

pub fn delete_password(id: &str) {
    if let Ok(e) = keyring::Entry::new(SERVICE, id) {
        let _ = e.delete_credential();
    }
}
