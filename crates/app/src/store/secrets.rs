//! Looks up/stores connection passwords in the OS keychain (macOS
//! Keychain, Windows Credential Manager, or Linux Secret Service/kwallet)
//! so [`db_core::ConnectionConfig`] — which is what gets written to disk
//! and exported — never carries a plaintext password.

use db_core::connection::ConnectionConfig;

const SERVICE: &str = "tableplus-rs";

fn entry(config: &ConnectionConfig) -> anyhow::Result<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, &config.secret_key())?)
}

pub fn get_password(config: &ConnectionConfig) -> anyhow::Result<Option<String>> {
    match entry(config)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn set_password(config: &ConnectionConfig, password: &str) -> anyhow::Result<()> {
    entry(config)?.set_password(password)?;
    Ok(())
}

pub fn delete_password(config: &ConnectionConfig) -> anyhow::Result<()> {
    match entry(config)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
