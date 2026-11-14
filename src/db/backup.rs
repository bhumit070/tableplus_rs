//! Backup / restore with the servers' own tools: `pg_dump` / `psql` and
//! `mysqldump` / `mysql` (or the MariaDB names). SQLite copies the file
//! with `VACUUM INTO`. Passwords go through the environment or a private
//! options file, never the command line.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use anyhow::{Context as _, Result, bail};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, BufWriter};
use tokio::process::{ChildStdin, Command};
use tokio::try_join;

use super::{ConnectionConfig, Driver};
use uuid::Uuid;

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
    // Windows installers put each version in its own folder.
    for parent in [r"C:\Program Files\PostgreSQL", r"C:\Program Files\MySQL"] {
        if let Ok(entries) = std::fs::read_dir(parent) {
            dirs.extend(entries.flatten().map(|e| e.path().join("bin")));
        }
    }
    names
        .iter()
        .map(|n| format!("{n}{}", std::env::consts::EXE_SUFFIX))
        .flat_map(|n| dirs.iter().map(move |d| d.join(&n)).collect::<Vec<_>>())
        .find(|p| p.is_file())
}

fn tool(names: &[&str], install: &str) -> Result<PathBuf> {
    find_tool(names).with_context(|| {
        let install = if cfg!(windows) {
            "the PostgreSQL / MySQL installer (command-line tools), then try again"
        } else if cfg!(target_os = "linux") {
            "your package manager (postgresql-client / mysql-client)"
        } else {
            install
        };
        format!("{} isn't installed. Install it with: {install}", names[0])
    })
}

fn pg_env(cmd: &mut Command, t: &Target) {
    cmd.env("PGPASSWORD", &t.cfg.password)
        .env("PGCONNECT_TIMEOUT", "15")
        .env(
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
        let p = std::env::temp_dir().join(format!("tabula-my-{}.cnf", Uuid::new_v4()));
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
        .arg("--protocol=TCP")
        .arg("--connect-timeout=15");
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
    check_exit(&out, what)
}

/// Fails with the tool's own message when it exited with an error.
fn check_exit(out: &Output, what: &str) -> Result<()> {
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{what} failed: {}", err.trim());
    }
    Ok(())
}

/// Runs psql reading the dump from stdin, which `feed_dump` fills.
async fn run_with_dump(mut cmd: Command, file: &Path) -> Result<()> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("couldn't start psql")?;
    // Owned out here so that when reading the file fails, psql is killed
    // (dropping its wait) before its input closes: an early end of input
    // would let `--single-transaction` commit a partial restore.
    let mut psql_input = Some(BufWriter::with_capacity(
        1 << 20,
        child.stdin.take().context("psql has no input")?,
    ));
    let (fed, out) = try_join!(feed_dump(file, &mut psql_input), async {
        child
            .wait_with_output()
            .await
            .context("psql stopped unexpectedly")
    })?;
    // psql stopping early (an error) closes its input; its message says why.
    check_exit(&out, "psql")?;
    if fed == Fed::Stopped {
        bail!("psql stopped reading the dump before its end");
    }
    Ok(())
}

/// Whether psql took the whole dump.
#[derive(Debug, PartialEq, Eq)]
enum Fed {
    Whole,
    Stopped,
}

/// Copies a pg_dump file into psql, with its version-dependent `SET`
/// lines made safe for the server it lands on (see `DumpLines`). psql
/// closing its input early is `Fed::Stopped`, not an error: whatever the
/// write error's kind, psql's own message is the one to show. The input
/// is taken (closing the pipe, which `shutdown` doesn't) only once the
/// whole file is read.
async fn feed_dump(file: &Path, psql_input: &mut Option<BufWriter<ChildStdin>>) -> Result<Fed> {
    let Some(input) = psql_input.as_mut() else {
        return Ok(Fed::Stopped);
    };
    let source = File::open(file)
        .await
        .with_context(|| format!("couldn't read {}", file.display()))?;
    let mut reader = BufReader::with_capacity(1 << 20, source);
    let mut dump_lines = DumpLines::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .await
            .with_context(|| format!("couldn't read {}", file.display()))?;
        if read == 0 {
            break;
        }
        if input.write_all(&dump_lines.rewrite(&line)).await.is_err() {
            return Ok(Fed::Stopped);
        }
    }
    let flushed = input.flush().await.is_ok();
    // Dropping it closes the pipe: psql sees the end and commits.
    drop(psql_input.take());
    Ok(if flushed { Fed::Whole } else { Fed::Stopped })
}

/// Settings pg_dump writes that older servers don't have (the version
/// that added each). pg_dump 17 always writes `SET transaction_timeout =
/// 0;`, which PostgreSQL 16 and older reject, failing the whole restore.
/// Any other `SET` goes through unchanged, so a typo or a setting the
/// user really needs still stops the restore.
const NEWER_SETTINGS: [&str; 5] = [
    "lock_timeout",                        // 9.3
    "row_security",                        // 9.5
    "idle_in_transaction_session_timeout", // 9.6
    "default_table_access_method",         // 12
    "transaction_timeout",                 // 17
];

/// Walks a dump line by line, rewriting its `NEWER_SETTINGS` lines
/// wherever they appear (pg_dump also sets `default_table_access_method`
/// before each table), except inside COPY data.
#[derive(Default)]
struct DumpLines {
    in_copy: bool,
}

impl DumpLines {
    fn rewrite<'a>(&mut self, line: &'a [u8]) -> Cow<'a, [u8]> {
        if self.in_copy {
            // COPY data ends with a line holding only `\.`.
            if line.trim_ascii_end() == b"\\." {
                self.in_copy = false;
            }
            return Cow::Borrowed(line);
        }
        let Ok(text) = str::from_utf8(line) else {
            return Cow::Borrowed(line);
        };
        // A file saved by some Windows editors starts with a BOM.
        let statement = text.trim_start_matches('\u{feff}').trim_end();
        if statement.starts_with("COPY ") && statement.ends_with("FROM stdin;") {
            self.in_copy = true;
            return Cow::Borrowed(line);
        }
        match portable_set(statement) {
            Some(guarded) => Cow::Owned(format!("{guarded}\n").into_bytes()),
            None => Cow::Borrowed(line),
        }
    }
}

/// `SET name = value;` for a `NEWER_SETTINGS` name becomes the same
/// setting applied only when the server has it. Only lines starting at
/// column 0, as pg_dump writes them; an indented `SET` is likely inside a
/// function body, where a SELECT would break it. `None` for anything else.
fn portable_set(line: &str) -> Option<String> {
    let rest = line.strip_prefix("SET ")?.strip_suffix(';')?;
    let (name, value) = rest.split_once(" = ")?;
    // Setting names are case-insensitive; pg_settings has them in lowercase.
    let name = name.to_ascii_lowercase();
    if !NEWER_SETTINGS.contains(&name.as_str()) {
        return None;
    }
    Some(guarded_set(&name, &setting_value(value.trim())?))
}

/// One setting value: a plain word or number, or one quoted string. Lists
/// (`'a', 'b'`), `E''` strings and anything else stay as they were.
fn setting_value(value: &str) -> Option<String> {
    if let Some(inner) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        // `''` is an escaped quote; a lone one means more than one string.
        if inner.replace("''", "").contains('\'') {
            return None;
        }
        return Some(inner.replace("''", "'"));
    }
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-+".contains(c));
    plain.then(|| value.to_string())
}

/// SQL applying setting `name` (lowercase) only if the server has it.
pub(super) fn guarded_set(name: &str, value: &str) -> String {
    format!(
        "SELECT pg_catalog.set_config('{name}', '{}', false) FROM pg_catalog.pg_settings WHERE name = '{name}';",
        value.replace('\'', "''")
    )
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
            // Into a temp file first: a failed backup mustn't cost the
            // previous one at the same path.
            let tmp = file.with_extension(format!("tmp-{}", Uuid::new_v4().simple()));
            let c = super::Connection::connect(&t.cfg).await?;
            let lit = format!("'{}'", tmp.to_string_lossy().replace('\'', "''"));
            let res = c
                .execute_cancellable(&format!("vacuum into {lit}"), &Default::default())
                .await;
            c.close().await;
            if let Err(e) = res {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
            std::fs::rename(&tmp, file)?;
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
                .arg("-");
            run_with_dump(cmd, file).await
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
    fn newer_settings_apply_only_where_the_server_has_them() {
        assert_eq!(
            portable_set("SET transaction_timeout = 0;").as_deref(),
            Some(
                "SELECT pg_catalog.set_config('transaction_timeout', '0', false) \
                 FROM pg_catalog.pg_settings WHERE name = 'transaction_timeout';"
            )
        );
        // Names are case-insensitive; pg_settings has them in lowercase.
        assert!(
            portable_set("SET Lock_Timeout = '5s';")
                .unwrap()
                .contains("set_config('lock_timeout', '5s', false) FROM pg_catalog.pg_settings WHERE name = 'lock_timeout'")
        );
        assert!(portable_set("SET default_table_access_method = heap;").is_some());
        // Every other setting stays a plain SET, so a typo still fails.
        assert_eq!(portable_set("SET client_encoding = 'UTF8';"), None);
        assert_eq!(portable_set("SET statment_timeout = 0;"), None);
        assert_eq!(portable_set("SET role = 'app_owner';"), None);
        assert_eq!(portable_set("CREATE SCHEMA analytics;"), None);
        assert_eq!(portable_set("SET lock_timeout = 0; DROP TABLE x;"), None);
        // Only one plain value or one quoted string.
        assert_eq!(portable_set("SET lock_timeout = '1s', '2s';"), None);
        assert_eq!(portable_set("SET lock_timeout = E'1s';"), None);
        assert!(
            portable_set("SET lock_timeout = 'it''s';")
                .unwrap()
                .contains("set_config('lock_timeout', 'it''s', false)")
        );
    }

    fn rewrite_dump(dump: &str) -> String {
        let mut dump_lines = DumpLines::default();
        dump.split_inclusive('\n')
            .map(|line| {
                String::from_utf8(dump_lines.rewrite(line.as_bytes()).into_owned()).unwrap()
            })
            .collect()
    }

    #[test]
    fn dump_settings_are_rewritten_everywhere_but_copy_data() {
        let dump = "\u{feff}SET transaction_timeout = 0;\n\
                    SET client_encoding = 'UTF8';\n\
                    CREATE TABLE public.t (note text);\n\
                    SET default_table_access_method = heap;\n\
                    COPY public.t (note) FROM stdin;\n\
                    SET lock_timeout = 0;\n\
                    \\.\n\
                    CREATE FUNCTION f() RETURNS void LANGUAGE sql AS $$\n    \
                    SET lock_timeout = 0;\n\
                    $$;\n\
                    SET lock_timeout = 0;\n";
        let out = rewrite_dump(dump);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("SELECT pg_catalog.set_config('transaction_timeout'"));
        assert_eq!(lines[1], "SET client_encoding = 'UTF8';");
        assert!(lines[3].starts_with("SELECT pg_catalog.set_config('default_table_access_method'"));
        assert_eq!(
            lines[5], "SET lock_timeout = 0;",
            "COPY data is copied as is"
        );
        assert_eq!(lines[8], "    SET lock_timeout = 0;", "function bodies too");
        assert!(lines[10].starts_with("SELECT pg_catalog.set_config('lock_timeout'"));
    }

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
