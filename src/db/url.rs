//! Reads a pasted connection URL into connection fields (design.md, screen
//! 04): `postgres://`, `mysql://`, `sqlserver://` (or `mssql://`),
//! `redis://`, `mongodb://` and `sqlite:`.
//! The password comes back as its own field so it only ever reaches the
//! keychain, never the saved host.

use anyhow::{Result, bail};

use super::Driver;
use anyhow::anyhow;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub driver: Option<Driver>,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub password: String,
    pub database: String,
    /// SQLite file.
    pub file: String,
    pub ssl: bool,
    pub ssl_verify: bool,
}

pub fn parse(url: &str) -> Result<Parsed> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("sqlite:") {
        let rest = &url["sqlite:".len()..];
        let rest = rest.strip_prefix("//").unwrap_or(rest);
        let path = rest.split('?').next().unwrap_or_default();
        if path.is_empty() {
            bail!("The URL has no file path");
        }
        return Ok(Parsed {
            driver: Some(Driver::Sqlite),
            file: decode(path),
            ..Default::default()
        });
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        bail!("Not a connection URL (expected e.g. postgres://user@host/db)");
    };
    let scheme = scheme.to_ascii_lowercase();
    let (driver, tls_scheme) = match scheme.as_str() {
        "postgres" | "postgresql" => (Driver::Postgres, false),
        "mysql" | "mariadb" => (Driver::MySql, false),
        "sqlserver" | "mssql" => (Driver::Mssql, false),
        "redis" => (Driver::Redis, false),
        "rediss" => (Driver::Redis, true),
        "mongodb" => (Driver::Mongo, false),
        "mongodb+srv" => {
            // DNS seed lists need the whole URI; keep it as the host, minus
            // the password.
            let (stripped, user, password) = strip_password(url);
            return Ok(Parsed {
                driver: Some(Driver::Mongo),
                host: stripped,
                user,
                password,
                ..Default::default()
            });
        }
        other => bail!("Unsupported URL scheme \"{other}\""),
    };

    let (before_query, query) = match rest.split_once('?') {
        Some((a, q)) => (a, q),
        None => (rest, ""),
    };
    let (authority, path) = match before_query.split_once('/') {
        Some((a, p)) => (a, p),
        None => (before_query, ""),
    };
    let (userinfo, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (u, h),
        None => ("", authority),
    };
    let (user, password) = match userinfo.split_once(':') {
        Some((u, p)) => (decode(u), decode(p)),
        None => (decode(userinfo), String::new()),
    };

    if hostport.contains(',') {
        if driver == Driver::Mongo {
            let (stripped, user, password) = strip_password(url);
            return Ok(Parsed {
                driver: Some(Driver::Mongo),
                host: stripped,
                user,
                password,
                ..Default::default()
            });
        }
        bail!(
            "Several hosts in one URL aren't supported for {}",
            driver.label()
        );
    }
    let (host, port) = split_host_port(hostport)?;

    let mut p = Parsed {
        driver: Some(driver),
        host: if host.is_empty() {
            "127.0.0.1".into()
        } else {
            host
        },
        port,
        user,
        password,
        database: decode(path.trim_matches('/')),
        ssl: tls_scheme,
        ..Default::default()
    };
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = decode(v).to_ascii_lowercase();
        match k.to_ascii_lowercase().as_str() {
            "sslmode" | "ssl-mode" => match v.as_str() {
                "require" | "required" | "prefer" | "preferred" => p.ssl = true,
                "verify-ca" | "verify-full" | "verify_ca" | "verify_identity" => {
                    p.ssl = true;
                    p.ssl_verify = true;
                }
                _ => {}
            },
            "ssl" | "tls" => p.ssl = v == "true" || v == "1",
            // SQL Server's connection-string words.
            "encrypt" => p.ssl = matches!(v.as_str(), "true" | "yes" | "mandatory" | "strict"),
            "trustservercertificate" => p.ssl_verify = matches!(v.as_str(), "false" | "no"),
            _ => {}
        }
    }
    Ok(p)
}

/// `host:port`, `[::1]:port` or a bare host.
fn split_host_port(s: &str) -> Result<(String, Option<u16>)> {
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let Some((h, after)) = rest.split_once(']') else {
            bail!("Unclosed [ in the host");
        };
        (h.to_string(), after.strip_prefix(':'))
    } else {
        match s.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (s.to_string(), None),
        }
    };
    let port = match port {
        Some("") | None => None,
        Some(p) => Some(
            p.parse()
                .map_err(|_| anyhow!("\"{p}\" isn't a port number"))?,
        ),
    };
    Ok((decode(&host), port))
}

/// The URL without its password, and the user and password it had.
fn strip_password(url: &str) -> (String, String, String) {
    let Some((scheme, rest)) = url.split_once("://") else {
        return (url.into(), String::new(), String::new());
    };
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let Some(at) = rest[..end].rfind('@') else {
        return (url.into(), String::new(), String::new());
    };
    let userinfo = &rest[..at];
    let (user, password) = match userinfo.split_once(':') {
        Some((u, p)) => (u, decode(p)),
        None => (userinfo, String::new()),
    };
    (
        format!("{scheme}://{user}@{}", &rest[at + 1..]),
        decode(user),
        password,
    )
}

/// Puts `password` back into a `mongodb://user@…` URI kept as the host.
pub fn with_password(uri: &str, password: &str) -> String {
    if password.is_empty() {
        return uri.into();
    }
    let Some((scheme, rest)) = uri.split_once("://") else {
        return uri.into();
    };
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) if !rest[..at].contains(':') => format!(
            "{scheme}://{}:{}@{}",
            &rest[..at],
            encode(password),
            &rest[at + 1..]
        ),
        _ => uri.into(),
    }
}

/// Percent-decoding; invalid escapes are kept as written.
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_with_everything() {
        let p =
            parse("postgresql://ada:p%40ss@db.example.com:6543/shop?sslmode=verify-full").unwrap();
        assert_eq!(
            p,
            Parsed {
                driver: Some(Driver::Postgres),
                host: "db.example.com".into(),
                port: Some(6543),
                user: "ada".into(),
                password: "p@ss".into(),
                database: "shop".into(),
                ssl: true,
                ssl_verify: true,
                ..Default::default()
            }
        );
    }

    #[test]
    fn sql_server_urls() {
        let p = parse(
            "sqlserver://sa:p%40ss@db.local:14330/shop?encrypt=true&trustServerCertificate=false",
        )
        .unwrap();
        assert_eq!(p.driver, Some(Driver::Mssql));
        assert_eq!((p.host.as_str(), p.port), ("db.local", Some(14330)));
        assert_eq!((p.user.as_str(), p.password.as_str()), ("sa", "p@ss"));
        assert_eq!(p.database, "shop");
        assert!(p.ssl && p.ssl_verify);
        assert_eq!(
            parse("mssql://sa@localhost").unwrap().driver,
            Some(Driver::Mssql)
        );
    }

    #[test]
    fn minimal_urls() {
        let p = parse("mysql://root@localhost").unwrap();
        assert_eq!(p.driver, Some(Driver::MySql));
        assert_eq!((p.host.as_str(), p.port), ("localhost", None));
        assert_eq!(p.user, "root");
        assert!(p.password.is_empty() && p.database.is_empty());

        let p = parse("rediss://:secret@[::1]:6380/2").unwrap();
        assert_eq!(p.driver, Some(Driver::Redis));
        assert_eq!((p.host.as_str(), p.port), ("::1", Some(6380)));
        assert_eq!((p.password.as_str(), p.database.as_str()), ("secret", "2"));
        assert!(p.ssl);
    }

    #[test]
    fn sqlite_paths() {
        assert_eq!(parse("sqlite:///tmp/a b.db").unwrap().file, "/tmp/a b.db");
        assert_eq!(
            parse("sqlite:data/app.db?mode=ro").unwrap().file,
            "data/app.db"
        );
        assert!(parse("sqlite:").is_err());
    }

    #[test]
    fn mongo_keeps_complex_uris_without_the_password() {
        let p = parse("mongodb+srv://ada:pw@cluster0.example.net/app?retryWrites=true").unwrap();
        assert_eq!(
            p.host,
            "mongodb+srv://ada@cluster0.example.net/app?retryWrites=true"
        );
        assert_eq!((p.user.as_str(), p.password.as_str()), ("ada", "pw"));
        assert_eq!(
            with_password(&p.host, "p@w"),
            "mongodb+srv://ada:p%40w@cluster0.example.net/app?retryWrites=true"
        );

        let p = parse("mongodb://ada:pw@a:27017,b:27017/?replicaSet=rs").unwrap();
        assert_eq!(p.host, "mongodb://ada@a:27017,b:27017/?replicaSet=rs");

        let p = parse("mongodb://ada:pw@h:27018/app").unwrap();
        assert_eq!((p.host.as_str(), p.port), ("h", Some(27018)));
    }

    #[test]
    fn rejects_what_it_cannot_read() {
        assert!(parse("ftp://x").is_err());
        assert!(parse("just text").is_err());
        assert!(parse("postgres://h:notaport/db").is_err());
        assert!(parse("postgres://a,b/db").is_err());
    }
}
