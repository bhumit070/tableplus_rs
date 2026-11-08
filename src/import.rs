//! Imports connections from TablePlus:
//! - an export file (`.tableplusconnection`): JSON, usually encrypted with a
//!   password (RNCryptor v3). It includes database passwords.
//! - TablePlus's own store on this Mac (`Connections.plist`), which doesn't;
//!   those passwords stay in TablePlus's Keychain items.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::db::ssh::{SshAuth, SshConfig};
use crate::db::{ConnectionConfig, Driver, EnvTag};
use crate::rncryptor;
use dirs::data_dir;
use plist::Value as PlistValue;
use serde_json::{from_slice, to_value};

#[derive(Debug, Default)]
pub struct ImportReport {
    pub added: Vec<ConnectionConfig>,
    /// Already imported earlier.
    pub existing: usize,
    /// Drivers Tabula doesn't support yet, e.g. "SQLServer".
    pub unsupported: Vec<String>,
    /// Imported SSH connections that log in with a private key: TablePlus
    /// exports don't include the key, so the default ~/.ssh keys are used.
    pub ssh_key: Vec<String>,
    /// Connections whose passwords (database or SSH) couldn't be stored in
    /// the keychain.
    pub keychain_failed: usize,
}

pub fn tableplus_store() -> PathBuf {
    data_dir()
        .unwrap_or_default()
        .join("com.tinyapp.TablePlus")
        .join("Data")
        .join("Connections.plist")
}

pub fn needs_password(bytes: &[u8]) -> bool {
    rncryptor::is_encrypted(bytes)
}

fn driver(name: &str) -> Option<Driver> {
    match name.to_ascii_lowercase().as_str() {
        "postgresql" | "postgres" | "redshift" | "cockroachdb" => Some(Driver::Postgres),
        "mysql" | "mariadb" => Some(Driver::MySql),
        "sqlserver" | "mssql" | "microsoft sql server" => Some(Driver::Mssql),
        "sqlite" | "libsql" => Some(Driver::Sqlite),
        "redis" => Some(Driver::Redis),
        "mongodb" | "mongo" => Some(Driver::Mongo),
        _ => None,
    }
}

fn s(d: &Map<String, Value>, k: &str) -> String {
    match d.get(k) {
        Some(Value::String(v)) => v.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn int(d: &Map<String, Value>, k: &str) -> i64 {
    match d.get(k) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(v)) => v.parse().unwrap_or(0),
        Some(Value::Bool(b)) => *b as i64,
        _ => 0,
    }
}

/// Stable id: TablePlus's own ID when present, else a fingerprint of the
/// connection's target, so importing the same file twice adds nothing.
fn stable_id(d: &Map<String, Value>, c: &ConnectionConfig) -> String {
    let tp = s(d, "ID");
    if !tp.is_empty() {
        return format!("tableplus-{tp}");
    }
    let fp = format!(
        "{:?}|{}|{}|{}|{}|{}|{}",
        c.driver, c.name, c.host, c.port, c.user, c.database, c.file
    );
    let hex: String = Sha256::digest(fp.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("tableplus-{hex}")
}

fn convert(d: &Map<String, Value>) -> std::result::Result<(ConnectionConfig, bool), String> {
    let drv = s(d, "Driver");
    let Some(driver) = driver(&drv) else {
        return Err(drv);
    };
    let mut c = ConnectionConfig::new(driver);
    c.name = s(d, "ConnectionName");
    let env = s(d, "Enviroment").to_ascii_lowercase();
    // Tabula has no Testing tag; Development is the closest.
    let env = if env == "testing" {
        "development".into()
    } else {
        env
    };
    c.tag = EnvTag::ALL
        .into_iter()
        .find(|t| t.label() == env)
        .unwrap_or_default();
    let host = s(d, "DatabaseHost");
    if !host.is_empty() {
        c.host = host;
    }
    if let Ok(p) = s(d, "DatabasePort").parse() {
        c.port = p;
    }
    c.user = s(d, "DatabaseUser");
    c.database = s(d, "DatabaseName");
    c.file = s(d, "DatabasePath");
    c.ssl = int(d, "tLSMode") != 0;
    // Only present in export files; never written to connections.json.
    c.password = d
        .get("DatabasePassword")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    c.id = stable_id(d, &c);
    let key = int(d, "isUsePrivateKey") != 0;
    if int(d, "isOverSSH") != 0 {
        c.ssh = Some(SshConfig {
            host: s(d, "ServerAddress"),
            port: s(d, "ServerPort").parse().unwrap_or(22),
            user: s(d, "ServerUser"),
            auth: if key { SshAuth::Key } else { SshAuth::Password },
            key_path: String::new(),
            secret: if key {
                String::new()
            } else {
                d.get("ServerPassword")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            },
        });
    }
    Ok((c, key && int(d, "isOverSSH") != 0))
}

/// Normalizes JSON or plist (a list of connections, or one) to JSON objects.
fn entries(plain: &[u8]) -> Result<Vec<Map<String, Value>>> {
    let value: Value = match from_slice(plain) {
        Ok(v) => v,
        Err(_) => {
            let p = PlistValue::from_reader(std::io::Cursor::new(plain))
                .context("not a TablePlus connection file")?;
            to_value(p)?
        }
    };
    let list = match value {
        Value::Array(items) => items,
        Value::Object(mut o) => match o.remove("Connections") {
            Some(Value::Array(items)) => items,
            _ => vec![Value::Object(o)],
        },
        _ => bail!("unexpected file layout"),
    };
    Ok(list
        .into_iter()
        .filter_map(|v| match v {
            Value::Object(o) => Some(o),
            _ => None,
        })
        .collect())
}

/// Same server/database/user, e.g. the same connection from the store and an export.
fn same_target(a: &ConnectionConfig, b: &ConnectionConfig) -> bool {
    (
        a.driver,
        &a.host,
        a.port,
        &a.user,
        &a.database,
        &a.file,
        &a.name,
    ) == (
        b.driver,
        &b.host,
        b.port,
        &b.user,
        &b.database,
        &b.file,
        &b.name,
    )
}

/// `password` is the export file's password, needed when `needs_password`.
pub fn parse(
    bytes: &[u8],
    password: Option<&str>,
    existing: &[ConnectionConfig],
) -> Result<ImportReport> {
    let decrypted;
    let plain = if needs_password(bytes) {
        let Some(pw) = password else {
            bail!("this file is password-protected")
        };
        decrypted = rncryptor::decrypt(bytes, pw)?;
        &decrypted[..]
    } else {
        bytes
    };
    let dicts = entries(plain)?;
    if dicts.is_empty() {
        bail!("no connections found in the file");
    }
    let mut report = ImportReport::default();
    for d in &dicts {
        match convert(d) {
            Ok((c, ssh)) => {
                if existing
                    .iter()
                    .chain(&report.added)
                    .any(|e| e.id == c.id || same_target(e, &c))
                {
                    report.existing += 1;
                    continue;
                }
                if ssh {
                    report.ssh_key.push(c.display_name());
                }
                report.added.push(c);
            }
            Err(drv) => report.unsupported.push(if drv.is_empty() {
                "unknown".into()
            } else {
                drv
            }),
        }
    }
    Ok(report)
}

impl ImportReport {
    pub fn summary(&self) -> String {
        // Counted per connection, as `keychain_failed` is.
        let with_pw = self
            .added
            .iter()
            .filter(|c| {
                !c.password.is_empty() || c.ssh.as_ref().is_some_and(|s| !s.secret.is_empty())
            })
            .count()
            .saturating_sub(self.keychain_failed);
        let mut parts = vec![format!("Imported {} connection(s)", self.added.len())];
        if with_pw > 0 {
            parts.push(format!("{with_pw} with passwords saved to the keychain"));
        }
        if self.existing > 0 {
            parts.push(format!("{} already imported", self.existing));
        }
        if !self.unsupported.is_empty() {
            let mut kinds = self.unsupported.clone();
            kinds.sort();
            kinds.dedup();
            parts.push(format!(
                "{} skipped (unsupported: {})",
                self.unsupported.len(),
                kinds.join(", ")
            ));
        }
        if !self.ssh_key.is_empty() {
            parts.push(format!(
                "{} use an SSH key: pick the key file when editing them (default: ~/.ssh)",
                self.ssh_key.len()
            ));
        }
        let mut out = parts.join(" · ") + ".";
        if self.added.len() > with_pw {
            out.push_str(" Connections without a password will ask for one when you connect.");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plist::{Dictionary, Value as PlistValue};
    use serde_json::json;
    use serde_json::to_string;

    fn conn(drv: &str, extra: Value) -> Value {
        let mut base = json!({
            "Driver": drv, "ConnectionName": format!("{drv} conn"), "DatabaseHost": "db.example.test",
            "DatabasePort": "6543", "DatabaseUser": "app", "DatabaseName": "main", "DatabasePath": "",
            "DatabasePassword": "", "Enviroment": "production", "tLSMode": 2, "isOverSSH": 0,
        });
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        base
    }

    #[test]
    fn maps_export_json_with_passwords() {
        let file = json!([
            conn("PostgreSQL", json!({"DatabasePassword": "s3cret"})),
            conn(
                "SQLite",
                json!({"DatabasePath": "/tmp/x.db", "Enviroment": "local"})
            ),
            conn(
                "MySQL",
                json!({"isOverSSH": 1, "tLSMode": 0, "DatabasePort": 3307,
                    "ServerAddress": "bastion.example.test", "ServerPort": "2222",
                    "ServerUser": "ops", "ServerPassword": "tunnel-pw", "isUsePrivateKey": 0})
            ),
            conn("SQLServer", json!({"DatabasePort": "1433"})),
            conn("Cassandra", json!({})),
        ]);
        let r = parse(file.to_string().as_bytes(), None, &[]).unwrap();
        assert_eq!(r.added.len(), 4);
        assert_eq!((r.added[3].driver, r.added[3].port), (Driver::Mssql, 1433));
        let pg = &r.added[0];
        assert_eq!(
            (pg.driver, pg.port, pg.tag, pg.ssl),
            (Driver::Postgres, 6543, EnvTag::Production, true)
        );
        assert_eq!(
            (pg.host.as_str(), pg.user.as_str(), pg.database.as_str()),
            ("db.example.test", "app", "main")
        );
        assert_eq!(pg.password, "s3cret");
        assert_eq!(r.added[1].file, "/tmp/x.db");
        assert_eq!((r.added[2].port, r.added[2].ssl), (3307, false));
        let ssh = r.added[2].ssh.as_ref().expect("ssh imported");
        assert_eq!(
            (ssh.host.as_str(), ssh.port, ssh.user.as_str(), ssh.auth),
            ("bastion.example.test", 2222, "ops", SshAuth::Password)
        );
        assert_eq!(ssh.secret, "tunnel-pw");
        assert!(r.ssh_key.is_empty());
        assert!(r.added[0].ssh.is_none());
        assert_eq!(r.unsupported, vec!["Cassandra"]);
        // The SSH password counts too, and failures are per connection.
        assert!(r.summary().contains("2 with passwords saved"));
        let r = ImportReport {
            keychain_failed: 1,
            ..r
        };
        assert!(r.summary().contains("1 with passwords saved"));
        // Passwords never reach connections.json.
        let json = to_string(&r.added).unwrap();
        assert!(!json.contains("s3cret") && !json.contains("tunnel-pw"));
    }

    #[test]
    fn reimport_is_idempotent() {
        let file = json!([conn("PostgreSQL", json!({}))]).to_string();
        let first = parse(file.as_bytes(), None, &[]).unwrap();
        let again = parse(file.as_bytes(), None, &first.added).unwrap();
        assert_eq!((again.added.len(), again.existing), (0, 1));
        // Same connection with a TablePlus ID (store) is still a duplicate.
        let with_id = json!([conn("PostgreSQL", json!({"ID": "X9"}))]).to_string();
        assert_eq!(
            parse(with_id.as_bytes(), None, &first.added)
                .unwrap()
                .existing,
            1
        );
    }

    #[test]
    fn encrypted_export_needs_the_right_password() {
        let file = rncryptor::encrypt(
            json!([conn("MySQL", json!({}))]).to_string().as_bytes(),
            "pw",
        );
        assert!(needs_password(&file));
        assert!(parse(&file, None, &[]).is_err());
        assert!(
            parse(&file, Some("wrong"), &[])
                .unwrap_err()
                .to_string()
                .contains("wrong password")
        );
        assert_eq!(parse(&file, Some("pw"), &[]).unwrap().added.len(), 1);
    }

    #[test]
    fn reads_plist_store_layout() {
        let mut d = Dictionary::new();
        d.insert("ID".into(), "A1".into());
        d.insert("Driver".into(), "PostgreSQL".into());
        d.insert("DatabaseHost".into(), "h".into());
        d.insert("DatabasePort".into(), "5432".into());
        d.insert("isOverSSH".into(), false.into());
        let mut bytes = Vec::new();
        PlistValue::Array(vec![d.into()])
            .to_writer_binary(&mut bytes)
            .unwrap();
        let r = parse(&bytes, None, &[]).unwrap();
        assert_eq!(r.added[0].id, "tableplus-A1");
        assert!(parse(b"not a connection file", None, &[]).is_err());
    }

    /// Dry run on the real files (counts only, nothing saved):
    /// TP_FILE=… TP_PW=… cargo test real_files -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_files() {
        let store = std::fs::read(tableplus_store()).unwrap();
        println!("store: {}", parse(&store, None, &[]).unwrap().summary());
        if let (Ok(f), Ok(pw)) = (std::env::var("TP_FILE"), std::env::var("TP_PW")) {
            let r = parse(&std::fs::read(f).unwrap(), Some(&pw), &[]).unwrap();
            let missing = r
                .added
                .iter()
                .filter(|c| c.driver != Driver::Sqlite && c.host.is_empty())
                .count();
            println!("export: {} (missing host: {missing})", r.summary());
        }
    }
}
