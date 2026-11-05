//! RNCryptor v3 (password-based) decryption — the open format TablePlus uses
//! for password-protected `.tableplusconnection` exports.
//!
//! Layout: version(0x03) | options(0x01) | encryption salt(8) | HMAC salt(8)
//!         | IV(16) | AES-256-CBC/PKCS7 ciphertext | HMAC-SHA256(32) over
//!         everything before it. Keys: PBKDF2-HMAC-SHA1, 10 000 rounds.

use anyhow::{Result, bail};
use cbc::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use sha2::Sha256;

const HEADER: usize = 2 + 8 + 8 + 16;
const MAC: usize = 32;
const ROUNDS: u32 = 10_000;

pub fn is_encrypted(data: &[u8]) -> bool {
    data.len() >= HEADER + MAC + 16 && data[0] == 3 && data[1] == 1
}

fn key(password: &str, salt: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha1>(password.as_bytes(), salt, ROUNDS, &mut k);
    k
}

pub fn decrypt(data: &[u8], password: &str) -> Result<Vec<u8>> {
    if !is_encrypted(data) {
        bail!("not a password-protected RNCryptor v3 file");
    }
    let (body, tag) = data.split_at(data.len() - MAC);
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&key(password, &data[10..18]))?;
    mac.update(body);
    if mac.verify_slice(tag).is_err() {
        bail!("wrong password (or the file is damaged)");
    }
    let enc_key = key(password, &data[2..10]);
    let iv = &data[18..34];
    cbc::Decryptor::<aes::Aes256>::new_from_slices(&enc_key, iv)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .decrypt_padded_vec::<Pkcs7>(&body[HEADER..])
        .map_err(|_| anyhow::anyhow!("couldn't decrypt the file"))
}

/// Encrypts per the same spec (fixed salts/IV), for tests only.
#[cfg(test)]
pub fn encrypt(plain: &[u8], password: &str) -> Vec<u8> {
    use cbc::cipher::BlockModeEncrypt;
    let (esalt, hsalt, iv) = ([1u8; 8], [2u8; 8], [3u8; 16]);
    let mut out = vec![3, 1];
    out.extend_from_slice(&esalt);
    out.extend_from_slice(&hsalt);
    out.extend_from_slice(&iv);
    let ct = cbc::Encryptor::<aes::Aes256>::new_from_slices(&key(password, &esalt), &iv)
        .unwrap()
        .encrypt_padded_vec::<Pkcs7>(plain);
    out.extend_from_slice(&ct);
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&key(password, &hsalt)).unwrap();
    mac.update(&out);
    out.extend_from_slice(&mac.finalize().into_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_wrong_password() {
        let data = encrypt(b"hello tabula, this spans more than one block", "pw");
        assert_eq!(
            decrypt(&data, "pw").unwrap(),
            b"hello tabula, this spans more than one block"
        );
        assert!(
            decrypt(&data, "nope")
                .unwrap_err()
                .to_string()
                .contains("wrong password")
        );
        let mut bad = data.clone();
        bad[40] ^= 1;
        assert!(decrypt(&bad, "pw").is_err());
    }
}
