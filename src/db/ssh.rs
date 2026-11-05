//! SSH tunnels: the database is reached through an SSH server, like
//! `ssh -L`. A local port on 127.0.0.1 forwards each connection over the
//! SSH session to the database host and port (as seen from the SSH server).
//!
//! Host keys are checked against `~/.ssh/known_hosts` and Tabula's own
//! `known_hosts`. An unknown key fails with `UnknownHostKey`, which the UI
//! shows so the user can trust it (stored in Tabula's file); a changed key
//! is always an error.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SshAuth {
    #[default]
    Password,
    /// A private key file (`key_path`; empty = the usual ~/.ssh/id_* files).
    Key,
    /// Keys held by the running ssh-agent.
    Agent,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub auth: SshAuth,
    #[serde(default)]
    pub key_path: String,
    /// Password, or the key's passphrase. Kept in the OS keychain.
    #[serde(skip)]
    pub secret: String,
}

/// The server's key isn't in any known_hosts file yet.
#[derive(Debug, Clone)]
pub struct UnknownHostKey {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    key: PublicKey,
}

impl std::fmt::Display for UnknownHostKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The authenticity of SSH host {}:{} can't be established. Its key fingerprint is {}.",
            self.host, self.port, self.fingerprint
        )
    }
}

impl std::error::Error for UnknownHostKey {}

impl UnknownHostKey {
    /// Remembers the key, so later connections accept it.
    pub fn trust(&self) -> Result<()> {
        russh::keys::known_hosts::learn_known_hosts_path(
            &self.host,
            self.port,
            &self.key,
            own_known_hosts(),
        )?;
        Ok(())
    }
}

fn own_known_hosts() -> PathBuf {
    crate::store::config_dir().join("known_hosts")
}

/// What `check_server_key` found, reported after the handshake fails.
#[derive(Debug)]
enum KeyProblem {
    Unknown(UnknownHostKey),
    Changed(String),
}

struct Client {
    host: String,
    port: u16,
    problem: Arc<Mutex<Option<KeyProblem>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            // Certificate host keys would need a trusted CA list.
            *self.problem.lock().unwrap() = Some(KeyProblem::Changed(
                "the server presented a host certificate, which isn't supported".into(),
            ));
            return Ok(false);
        };
        let mut files = vec![own_known_hosts()];
        if !cfg!(test)
            && let Some(home) = dirs::home_dir()
        {
            files.insert(0, home.join(".ssh").join("known_hosts"));
        }
        for file in files {
            if !file.exists() {
                continue;
            }
            match russh::keys::known_hosts::check_known_hosts_path(
                &self.host, self.port, key, &file,
            ) {
                Ok(true) => return Ok(true),
                Ok(false) => {}
                Err(russh::keys::Error::KeyChanged { line }) => {
                    *self.problem.lock().unwrap() = Some(KeyProblem::Changed(format!(
                        "the SSH host key for {}:{} has CHANGED (it doesn't match line {line} \
                         of {}). Someone could be intercepting the connection. If the server \
                         was reinstalled, remove that line and connect again.",
                        self.host,
                        self.port,
                        file.display()
                    )));
                    return Ok(false);
                }
                // An unreadable line or file: keep looking elsewhere.
                Err(_) => {}
            }
        }
        *self.problem.lock().unwrap() = Some(KeyProblem::Unknown(UnknownHostKey {
            host: self.host.clone(),
            port: self.port,
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            key: key.clone(),
        }));
        Ok(false)
    }
}

/// An open tunnel; dropping it closes the listener and the SSH session.
pub struct Tunnel {
    pub port: u16,
    pub host: String,
    task: tokio::task::JoinHandle<()>,
    session: Arc<client::Handle<Client>>,
}

impl Tunnel {
    /// The SSH session ended (server gone, network dropped, keepalive
    /// failed): nothing goes through any more.
    pub fn closed(&self) -> bool {
        self.session.is_closed() || self.task.is_finished()
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(p),
    }
}

impl Tunnel {
    /// Connects and authenticates to the SSH server, then forwards a new
    /// local port to `target_host:target_port`.
    pub async fn open(ssh: &SshConfig, target_host: &str, target_port: u16) -> Result<Tunnel> {
        if target_host.starts_with("mongodb") {
            bail!("a MongoDB connection string can't go through an SSH tunnel; enter host and port instead");
        }
        let port = if ssh.port == 0 { 22 } else { ssh.port };
        let config = Arc::new(client::Config {
            keepalive_interval: Some(Duration::from_secs(30)),
            nodelay: true,
            ..Default::default()
        });
        let problem = Arc::new(Mutex::new(None));
        let handler = Client {
            host: ssh.host.clone(),
            port,
            problem: problem.clone(),
        };
        let connect = client::connect(config, (ssh.host.as_str(), port), handler);
        let mut session = match tokio::time::timeout(Duration::from_secs(15), connect).await {
            Err(_) => bail!("SSH: timed out connecting to {}:{port}", ssh.host),
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                return Err(match problem.lock().unwrap().take() {
                    Some(KeyProblem::Unknown(u)) => anyhow::Error::new(u),
                    Some(KeyProblem::Changed(msg)) => anyhow::anyhow!("SSH: {msg}"),
                    None => anyhow::Error::new(e)
                        .context(format!("SSH: couldn't connect to {}:{port}", ssh.host)),
                });
            }
        };
        // An agent waiting for a hardware-key touch, or a stalled server,
        // mustn't hang the connect forever.
        tokio::time::timeout(Duration::from_secs(60), authenticate(&mut session, ssh))
            .await
            .map_err(|_| anyhow::anyhow!("SSH: login to {} didn't finish within 60 s", ssh.host))??;

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let local_port = listener.local_addr()?.port();
        let session = Arc::new(session);
        let kept = session.clone();
        let target = target_host.to_string();
        let task = tokio::spawn(async move {
            // Owned here, so aborting the task also stops every forward.
            let mut forwards = JoinSet::new();
            while let Ok((mut socket, from)) = listener.accept().await {
                let (session, target) = (session.clone(), target.clone());
                forwards.spawn(async move {
                    let channel = session
                        .channel_open_direct_tcpip(
                            target,
                            target_port as u32,
                            from.ip().to_string(),
                            from.port() as u32,
                        )
                        .await;
                    match channel {
                        Ok(channel) => {
                            let mut stream = channel.into_stream();
                            let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                        }
                        // Closing the socket makes the driver fail; the log
                        // says it was the tunnel (the driver can't know).
                        Err(e) => eprintln!("SSH tunnel: couldn't open a channel to the database: {e}"),
                    }
                });
                // Reap finished forwards so the set doesn't grow forever.
                while forwards.try_join_next().is_some() {}
            }
        });
        Ok(Tunnel {
            port: local_port,
            host: ssh.host.clone(),
            task,
            session: kept,
        })
    }
}

async fn authenticate(session: &mut client::Handle<Client>, ssh: &SshConfig) -> Result<()> {
    let user = ssh.user.clone();
    let ok = match ssh.auth {
        SshAuth::Password => session
            .authenticate_password(user, ssh.secret.clone())
            .await?
            .success(),
        SshAuth::Key => {
            let paths: Vec<PathBuf> = if ssh.key_path.trim().is_empty() {
                let home = dirs::home_dir().unwrap_or_default().join(".ssh");
                ["id_ed25519", "id_ecdsa", "id_rsa"]
                    .iter()
                    .map(|f| home.join(f))
                    .filter(|p| p.exists())
                    .collect()
            } else {
                vec![expand_home(ssh.key_path.trim())]
            };
            if paths.is_empty() {
                bail!("SSH: no private key found in ~/.ssh; choose a key file");
            }
            let passphrase = (!ssh.secret.is_empty()).then_some(ssh.secret.as_str());
            let mut ok = false;
            for path in paths {
                let key = russh::keys::load_secret_key(&path, passphrase)
                    .with_context(|| format!("SSH: couldn't read key {}", path.display()))?;
                let hash = session.best_supported_rsa_hash().await?.flatten();
                let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
                if session
                    .authenticate_publickey(user.clone(), key)
                    .await?
                    .success()
                {
                    ok = true;
                    break;
                }
            }
            ok
        }
        SshAuth::Agent => {
            let mut agent = russh::keys::agent::client::AgentClient::connect_env()
                .await
                .context("SSH: couldn't reach ssh-agent (is SSH_AUTH_SOCK set?)")?;
            let ids = agent.request_identities().await?;
            let hash = session.best_supported_rsa_hash().await?.flatten();
            let mut ok = false;
            for id in ids {
                let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = id else {
                    continue;
                };
                if let Ok(r) = session
                    .authenticate_publickey_with(user.clone(), key, hash, &mut agent)
                    .await
                    && r.success()
                {
                    ok = true;
                    break;
                }
            }
            ok
        }
    };
    if !ok {
        bail!("SSH: authentication failed for user {}", ssh.user);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_server {
    //! A minimal in-process SSH server that allows password login and
    //! forwards direct-tcpip channels, for tests.

    use std::sync::Arc;

    use russh::keys::{Algorithm, PrivateKey};
    use russh::Channel;
    use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};
    use tokio::net::{TcpListener, TcpStream};

    pub const USER: &str = "tunnel";
    pub const PASSWORD: &str = "secret";

    #[derive(Clone)]
    struct Server {
        /// Accepted for public-key login, if set.
        client_key: Option<russh::keys::PublicKey>,
    }

    impl server::Handler for Server {
        type Error = russh::Error;

        async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
            Ok(if user == USER && password == PASSWORD {
                Auth::Accept
            } else {
                Auth::reject()
            })
        }

        async fn auth_publickey(
            &mut self,
            user: &str,
            key: russh::server::VerifiedPublicKey<'_>,
        ) -> Result<Auth, Self::Error> {
            let ok = user == USER && self.client_key.as_ref() == Some(key.public_key);
            Ok(if ok { Auth::Accept } else { Auth::reject() })
        }

        async fn channel_open_direct_tcpip(
            &mut self,
            channel: Channel<Msg>,
            host: &str,
            port: u32,
            _: &str,
            _: u32,
            reply: ChannelOpenHandle,
            _: &mut Session,
        ) -> Result<(), Self::Error> {
            let target = format!("{host}:{port}");
            reply.accept().await;
            tokio::spawn(async move {
                if let Ok(mut tcp) = TcpStream::connect(target).await {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
                }
            });
            Ok(())
        }
    }

    /// Starts the server on a free port; returns (port, host key).
    pub async fn start(client_key: Option<russh::keys::PublicKey>) -> (u16, PrivateKey) {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let config = Arc::new(server::Config {
            keys: vec![key.clone()],
            auth_rejection_time: std::time::Duration::from_millis(10),
            ..Default::default()
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (config, client_key) = (config.clone(), client_key.clone());
                tokio::spawn(async move {
                    let handler = Server { client_key };
                    if let Ok(s) = server::run_stream(config, socket, handler).await {
                        let _ = s.await;
                    }
                });
            }
        });
        (port, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn cfg(port: u16, password: &str) -> SshConfig {
        SshConfig {
            host: "127.0.0.1".into(),
            port,
            user: test_server::USER.into(),
            auth: SshAuth::Password,
            key_path: String::new(),
            secret: password.into(),
        }
    }

    /// Unknown key → `UnknownHostKey`; after `trust`, the tunnel forwards
    /// bytes both ways; a wrong password fails cleanly.
    #[tokio::test]
    async fn tunnel_trusts_on_request_and_forwards() {
        let (ssh_port, _) = test_server::start(None).await;
        // An echo server stands in for the database.
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });

        let err = Tunnel::open(&cfg(ssh_port, test_server::PASSWORD), "127.0.0.1", echo_port)
            .await
            .err()
            .expect("unknown host key is refused");
        let unknown = err.downcast_ref::<UnknownHostKey>().expect("UnknownHostKey");
        assert!(unknown.fingerprint.starts_with("SHA256:"));
        unknown.trust().unwrap();

        let bad = Tunnel::open(&cfg(ssh_port, "nope"), "127.0.0.1", echo_port).await;
        assert!(format!("{:#}", bad.err().unwrap()).contains("authentication failed"));

        let t = Tunnel::open(&cfg(ssh_port, test_server::PASSWORD), "127.0.0.1", echo_port)
            .await
            .unwrap();
        for msg in [&b"hello"[..], b"second connection"] {
            let mut c = tokio::net::TcpStream::connect(("127.0.0.1", t.port)).await.unwrap();
            c.write_all(msg).await.unwrap();
            let mut buf = vec![0; msg.len()];
            c.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, msg);
        }
        let port = t.port;
        drop(t);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err(),
            "closed with the connection"
        );
    }

    /// Key-file login, with a passphrase-protected OpenSSH key.
    #[tokio::test]
    async fn tunnel_logs_in_with_a_key_file() {
        use russh::keys::{Algorithm, PrivateKey};
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let (ssh_port, _) = test_server::start(Some(key.public_key().clone())).await;
        let file = std::env::temp_dir().join(format!("tabula-ssh-key-{}", uuid::Uuid::new_v4()));
        let locked = key.encrypt(&mut rand::rng(), "pass phrase").unwrap();
        std::fs::write(&file, locked.to_openssh(Default::default()).unwrap().as_bytes()).unwrap();
        let mut c = cfg(ssh_port, "pass phrase");
        c.auth = SshAuth::Key;
        c.key_path = file.to_string_lossy().into();
        // Trust the host first.
        if let Err(e) = Tunnel::open(&c, "127.0.0.1", 9).await {
            e.downcast_ref::<UnknownHostKey>().unwrap().trust().unwrap();
        }
        assert!(Tunnel::open(&c, "127.0.0.1", 9).await.is_ok());
        c.secret = "wrong".into();
        assert!(Tunnel::open(&c, "127.0.0.1", 9).await.is_err());
        let _ = std::fs::remove_file(file);
    }
}
