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
    read_json(p, "saved connections")
}

/// Files that couldn't be read (and weren't moved aside): saving over them
/// would destroy what's there, so writes to them are refused.
static PROTECTED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Reads a JSON file Tabula owns. Missing → default, no warning. Unparsable
/// (hand edit, newer format…) → moved aside so the next save can't destroy
/// it. Unreadable (permissions…) → default, and saves to it are refused.
/// The message says what happened.
pub fn read_json<T: serde::de::DeserializeOwned + Default>(
    p: &std::path::Path,
    what: &str,
) -> (T, Option<String>) {
    let text = match std::fs::read_to_string(p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (T::default(), None),
        Err(e) => {
            PROTECTED.lock().unwrap_or_else(|e| e.into_inner()).push(p.to_path_buf());
            return (
                T::default(),
                Some(format!(
                    "Couldn't read {what} ({e}) at {}. Changes won't be saved until it's readable.",
                    p.display()
                )),
            );
        }
    };
    match serde_json::from_str(&text) {
        Ok(v) => (v, None),
        Err(e) => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("json");
            let backup = p.with_extension(format!("{ext}.unreadable-{stamp}"));
            let kept = match std::fs::rename(p, &backup) {
                Ok(()) => format!("It was kept as {}", backup.display()),
                Err(re) => {
                    PROTECTED.lock().unwrap_or_else(|e| e.into_inner()).push(p.to_path_buf());
                    format!("It couldn't be moved aside ({re}), so changes won't be saved over it")
                }
            };
            (T::default(), Some(format!("Couldn't read {what} ({e}). {kept}.")))
        }
    }
}

/// Writes a temp file and renames it over the old one, so a crash mid-write
/// can't leave a truncated file behind.
pub fn write_atomic(p: &std::path::Path, text: &str) -> Result<()> {
    if PROTECTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|x| x == p)
    {
        anyhow::bail!("not saved: {} couldn't be read earlier, so it isn't overwritten", p.display());
    }
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("tmp");
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        // Flush to disk before the rename, so a crash can't leave the
        // renamed file empty.
        f.sync_all()?;
    }
    std::fs::rename(&tmp, p)?;
    Ok(())
}

/// Passwords are read lazily, only when a connection is opened. No entry =
/// no password; any other keychain failure (locked, access denied…) is an
/// error, so it can't pass for an empty password.
fn password(id: &str) -> Result<String> {
    // Tests never touch the real Keychain.
    if cfg!(test) {
        return Ok(String::new());
    }
    match keyring::Entry::new(SERVICE, id).and_then(|e| e.get_password()) {
        Ok(p) => Ok(p),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(e) => Err(anyhow::anyhow!("Keychain: couldn't read the saved password ({e})")),
    }
}

/// Keychain account for a connection's SSH password / key passphrase.
fn ssh_account(id: &str) -> String {
    format!("{id}:ssh")
}

/// Fills in a connection's secrets (database password, SSH password or
/// passphrase) from the keychain, where they aren't set already.
pub fn load_secrets(cfg: &mut ConnectionConfig) -> Result<()> {
    // SQLite files have no password: don't depend on the keychain at all.
    if cfg.driver == crate::db::Driver::Sqlite {
        return Ok(());
    }
    if cfg.password.is_empty() {
        cfg.password = password(&cfg.id)?;
    }
    if let Some(ssh) = &mut cfg.ssh
        && ssh.secret.is_empty()
    {
        ssh.secret = password(&ssh_account(&cfg.id))?;
    }
    Ok(())
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

fn save_to(p: &std::path::Path, list: &[ConnectionConfig]) -> Result<()> {
    write_atomic(p, &serde_json::to_string_pretty(list)?)
}

fn set_password(id: &str, password: &str) -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    let entry = keyring::Entry::new(SERVICE, id).map_err(keychain)?;
    if password.is_empty() {
        delete(&entry)
    } else {
        entry.set_password(password).map_err(keychain)
    }
}

fn keychain(e: keyring::Error) -> anyhow::Error {
    anyhow::anyhow!("Keychain: {e}")
}

/// Deleting what isn't there is fine; anything else is reported.
fn delete(entry: &keyring::Entry) -> Result<()> {
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(keychain(e)),
    }
}

/// Removes a deleted connection's secrets.
pub fn delete_password(id: &str) -> Result<()> {
    if cfg!(test) {
        return Ok(());
    }
    for account in [id.to_string(), ssh_account(id)] {
        delete(&keyring::Entry::new(SERVICE, &account).map_err(keychain)?)?;
    }
    Ok(())
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
