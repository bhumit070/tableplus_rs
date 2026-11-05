//! Backup / restore with the servers' own tools: `pg_dump` / `psql` and
//! `mysqldump` / `mysql` (or the MariaDB names). SQLite copies the file
//! with `VACUUM INTO`. Passwords go through the environment or a private
//! options file, never the command line.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context as _, Result, bail};
use tokio::process::Command;

use super::{ConnectionConfig, Driver};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Everything,
    SchemaOnly,
    DataOnly,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Everything, Scope::SchemaOnly, Scope::DataOnly];

    pub fn label(self) -> &'static str {
        match self {
            Scope::Everything => "Structure and data",
            Scope::SchemaOnly => "Structure only",
            Scope::DataOnly => "Data only",
        }
    }
}

/// Where the server is reachable from here (the SSH tunnel's local end
/// when there is one) plus what's needed to log in.
#[derive(Clone, Debug)]
pub struct Target {
    pub cfg: ConnectionConfig,
    pub host: String,
    pub port: u16,
    pub database: String,
}

/// Apps started from the Dock don't get the shell's PATH, so look in the
/// usual install places too.
pub fn find_tool(names: &[&str]) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for d in [
        "/opt/homebrew/bin",
        "/opt/homebrew/opt/libpq/bin",
        "/opt/homebrew/opt/mysql-client/bin",
        "/usr/local/bin",
        "/usr/local/opt/libpq/bin",
        "/usr/local/opt/mysql-client/bin",
        "/Applications/Postgres.app/Contents/Versions/latest/bin",
        "/usr/local/mysql/bin",
        "/usr/bin",
    ] {
        dirs.push(d.into());
    }
    names
        .iter()
        .flat_map(|n| dirs.iter().map(move |d| d.join(n)))
        .find(|p| p.is_file())
}

fn tool(names: &[&str], install: &str) -> Result<PathBuf> {
    find_tool(names).with_context(|| {
        format!("{} isn't installed. Install it with: {install}", names[0])
    })
}

fn pg_env(cmd: &mut Command, t: &Target) {
    cmd.env("PGPASSWORD", &t.cfg.password).env(
        "PGSSLMODE",
        match (t.cfg.ssl, t.cfg.ssl_verify) {
            (true, true) => "verify-full",
            (true, false) => "require",
            _ => "prefer",
        },
    );
}

/// A `[client]` options file readable only by us, holding the password.
struct MyOptions(PathBuf);

impl MyOptions {
    fn new(t: &Target) -> Result<MyOptions> {
        let p = std::env::temp_dir().join(format!("tabula-my-{}.cnf", uuid::Uuid::new_v4()));
        let esc = t.cfg.password.replace('\\', "\\\\").replace('"', "\\\"");
        let body = format!("[client]\npassword=\"{esc}\"\n");
        #[cfg(unix)]
        {
            use std::io::Write as _;
            use std::os::unix::fs::OpenOptionsExt as _;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&p)?;
            f.write_all(body.as_bytes())?;
        }
        #[cfg(not(unix))]
        std::fs::write(&p, body)?;
        Ok(MyOptions(p))
    }
}

impl Drop for MyOptions {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn my_args(cmd: &mut Command, t: &Target, opts: &MyOptions) {
    // Must come first.
    cmd.arg(format!("--defaults-extra-file={}", opts.0.display()))
        .arg(format!("--host={}", t.host))
        .arg(format!("--port={}", t.port))
        .arg(format!("--user={}", t.cfg.user))
        .arg("--protocol=TCP");
    if t.cfg.ssl {
        cmd.arg(if t.cfg.ssl_verify {
            "--ssl-mode=VERIFY_IDENTITY"
        } else {
            "--ssl-mode=REQUIRED"
        });
    }
}

async fn run(mut cmd: Command, what: &str) -> Result<()> {
    let out = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("couldn't start {what}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{what} failed: {}", err.trim());
    }
    Ok(())
}

/// Writes a SQL dump of the database to `file`.
pub async fn dump(t: &Target, scope: Scope, file: &Path) -> Result<()> {
    match t.cfg.driver {
        Driver::Postgres => {
            let mut cmd = Command::new(tool(&["pg_dump"], "brew install libpq")?);
            pg_env(&mut cmd, t);
            cmd.arg("--host")
                .arg(&t.host)
                .arg("--port")
                .arg(t.port.to_string())
                .arg("--username")
                .arg(&t.cfg.user)
                .arg("--no-password")
                .arg("--format=plain")
                .arg("--no-owner")
                .arg("--file")
                .arg(file)
                .arg(&t.database);
            match scope {
                Scope::SchemaOnly => {
                    cmd.arg("--schema-only");
                }
                Scope::DataOnly => {
                    cmd.arg("--data-only");
                }
                Scope::Everything => {}
            }
            run(cmd, "pg_dump").await
        }
        Driver::MySql => {
            let opts = MyOptions::new(t)?;
            let mut cmd = Command::new(tool(
                &["mysqldump", "mariadb-dump"],
                "brew install mysql-client",
            )?);
            my_args(&mut cmd, t, &opts);
            cmd.arg("--single-transaction")
                .arg("--routines")
                .arg("--triggers")
                .arg("--no-tablespaces")
                .arg(format!("--result-file={}", file.display()));
            match scope {
                Scope::SchemaOnly => {
                    cmd.arg("--no-data");
                }
                Scope::DataOnly => {
                    cmd.arg("--no-create-info");
                }
                Scope::Everything => {}
            }
            cmd.arg(&t.database);
            run(cmd, "mysqldump").await
        }
        Driver::Sqlite => {
            if scope != Scope::Everything {
                bail!("SQLite backups copy the whole file");
            }
            if file.exists() {
                std::fs::remove_file(file)?;
            }
            let c = super::Connection::connect(&t.cfg).await?;
            let lit = format!("'{}'", file.to_string_lossy().replace('\'', "''"));
            c.execute_cancellable(&format!("vacuum into {lit}"), &Default::default())
                .await?;
            c.close().await;
            Ok(())
        }
        _ => bail!("backups aren't available for {}", t.cfg.driver.label()),
    }
}

/// Runs a SQL dump file against the database: all or nothing on Postgres
/// (one transaction), stops at the first error on MySQL.
pub async fn restore(t: &Target, file: &Path) -> Result<()> {
    match t.cfg.driver {
        Driver::Postgres => {
            let mut cmd = Command::new(tool(&["psql"], "brew install libpq")?);
            pg_env(&mut cmd, t);
            cmd.arg("--host")
                .arg(&t.host)
                .arg("--port")
                .arg(t.port.to_string())
                .arg("--username")
                .arg(&t.cfg.user)
                .arg("--no-password")
                .arg("--dbname")
                .arg(&t.database)
                .arg("--single-transaction")
                .arg("--set=ON_ERROR_STOP=1")
                .arg("--quiet")
                .arg("--file")
                .arg(file);
            run(cmd, "psql").await
        }
        Driver::MySql => {
            let opts = MyOptions::new(t)?;
            let mut cmd = Command::new(tool(&["mysql", "mariadb"], "brew install mysql-client")?);
            my_args(&mut cmd, t, &opts);
            // `source` reads the file itself, so no shell redirect is needed.
            let path = file.to_string_lossy().replace('\\', "\\\\");
            cmd.arg(format!("--database={}", t.database))
                .arg(format!("--execute=source {path}"));
            run(cmd, "mysql").await
        }
        _ => bail!("restore isn't available for {}", t.cfg.driver.label()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_password_file_is_private_and_removed() {
        let mut cfg = ConnectionConfig::new(Driver::MySql);
        cfg.password = r#"p"a\ss"#.into();
        let t = Target {
            cfg,
            host: "h".into(),
            port: 1,
            database: "d".into(),
        };
        let path = {
            let o = MyOptions::new(&t).unwrap();
            let text = std::fs::read_to_string(&o.0).unwrap();
            assert_eq!(text, "[client]\npassword=\"p\\\"a\\\\ss\"\n");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = std::fs::metadata(&o.0).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600);
            }
            o.0.clone()
        };
        assert!(!path.exists(), "deleted after use");
    }
}
