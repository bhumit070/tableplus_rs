//! Saved connections: settings in `connections.json`, passwords in the OS
//! keychain (service "tabula", account = connection id).

use std::path::PathBuf;

use anyhow::Result;

use crate::db::ConnectionConfig;

const SERVICE: &str = "tabula";

/// Where Tabula keeps its files. Tests get a throwaway folder, so they can
/// never read or change the user's real connections or settings.
pub fn config_dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir().join(format!("tabula-test-config-{}", std::process::id()));
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tabula")
}

fn path() -> PathBuf {
    config_dir().join("connections.json")
}

/// Loads saved connections. A file that can't be parsed (hand edit, newer
/// format…) is moved aside, never silently replaced by the next save; the
/// returned message says where it went.
pub fn load() -> (Vec<ConnectionConfig>, Option<String>) {
    load_from(&path())
}

fn load_from(p: &std::path::Path) -> (Vec<ConnectionConfig>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(p) else {
        return (vec![], None);
    };
    match serde_json::from_str(&text) {
        Ok(list) => (list, None),
        Err(e) => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let backup = p.with_extension(format!("json.unreadable-{stamp}"));
            let kept = match std::fs::rename(p, &backup) {
                Ok(()) => format!("It was kept as {}", backup.display()),
                Err(_) => "It was left in place".into(),
            };
            (
                vec![],
                Some(format!("Couldn't read saved connections ({e}). {kept}.")),
            )
        }
    }
}

/// Passwords are read lazily, only when a connection is opened.
fn password(id: &str) -> String {
    // Tests never touch the real Keychain.
    if cfg!(test) {
        return String::new();
    }
    keyring::Entry::new(SERVICE, id)
        .and_then(|e| e.get_password())
        .unwrap_or_default()
}

/// Keychain account for a connection's SSH password / key passphrase.
fn ssh_account(id: &str) -> String {
    format!("{id}:ssh")
}

/// Fills in a connection's secrets (database password, SSH password or
/// passphrase) from the keychain, where they aren't set already.
pub fn load_secrets(cfg: &mut ConnectionConfig) {
    if cfg.password.is_empty() {
        cfg.password = password(&cfg.id);
    }
    if let Some(ssh) = &mut cfg.ssh
        && ssh.secret.is_empty()
    {
        ssh.secret = password(&ssh_account(&cfg.id));
    }
}

/// Stores a connection's secrets in the keychain (empty ones are removed).
pub fn save_secrets(cfg: &ConnectionConfig) -> Result<()> {
    set_password(&cfg.id, &cfg.password)?;
    let ssh = cfg.ssh.as_ref().map(|s| s.secret.as_str()).unwrap_or("");
    set_password(&ssh_account(&cfg.id), ssh)
}

pub fn save_all(list: &[ConnectionConfig]) -> Result<()> {
    save_to(&path(), list)
}

/// Writes a temp file and renames it over the old one, so a crash mid-write
/// can't leave a truncated file behind.
fn save_to(p: &std::path::Path, list: &[ConnectionConfig]) -> Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(list)?)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

fn set_password(id: &str, password: &str) -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    let entry = keyring::Entry::new(SERVICE, id)?;
    if password.is_empty() {
        let _ = entry.delete_credential();
    } else {
        entry.set_password(password)?;
    }
    Ok(())
}

pub fn delete_password(id: &str) {
    if cfg!(test) {
        return;
    }
    for account in [id.to_string(), ssh_account(id)] {
        if let Ok(e) = keyring::Entry::new(SERVICE, &account) {
            let _ = e.delete_credential();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Driver;

    #[test]
    fn unreadable_file_is_kept_aside_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("tabula-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("connections.json");

        let mut c = ConnectionConfig::new(Driver::Sqlite);
        c.name = "kept".into();
        save_to(&p, &[c]).unwrap();
        assert_eq!(load_from(&p).0[0].name, "kept");

        std::fs::write(&p, "[{ broken").unwrap();
        let (list, warning) = load_from(&p);
        assert!(list.is_empty());
        assert!(warning.unwrap().contains("unreadable-"));
        // The bad file was moved, so saving can't destroy it.
        let backups: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("unreadable-"))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            "[{ broken"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
