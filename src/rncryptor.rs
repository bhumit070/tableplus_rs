//! RNCryptor v3 (password-based) decryption — the open format TablePlus uses
//! for password-protected `.tableplusconnection` exports.
//!
//! Layout: version(0x03) | options(0x01) | encryption salt(8) | HMAC salt(8)
//!         | IV(16) | AES-256-CBC/PKCS7 ciphertext | HMAC-SHA256(32) over
//!         everything before it. Keys: PBKDF2-HMAC-SHA1, 10 000 rounds.

use aes::Aes256;
use anyhow::anyhow;
use anyhow::{Result, bail};
use cbc::Decryptor;
use cbc::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use hmac::{Hmac, KeyInit, Mac};
use pbkdf2::pbkdf2_hmac;
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
    pbkdf2_hmac::<Sha1>(password.as_bytes(), salt, ROUNDS, &mut k);
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
    Decryptor::<Aes256>::new_from_slices(&enc_key, iv)
        .map_err(|e| anyhow!("{e}"))?
        .decrypt_padded_vec::<Pkcs7>(&body[HEADER..])
        .map_err(|_| anyhow!("couldn't decrypt the file"))
}

/// Encrypts per the same spec (fixed salts/IV), for tests only.
#[cfg(test)]
pub fn encrypt(plain: &[u8], password: &str) -> Vec<u8> {
    use cbc::Encryptor;
    use cbc::cipher::BlockModeEncrypt;
    let (esalt, hsalt, iv) = ([1u8; 8], [2u8; 8], [3u8; 16]);
    let mut out = vec![3, 1];
    out.extend_from_slice(&esalt);
    out.extend_from_slice(&hsalt);
    out.extend_from_slice(&iv);
    let ct = Encryptor::<Aes256>::new_from_slices(&key(password, &esalt), &iv)
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

    /// The official RNCryptor v3 password vectors
    /// (github.com/RNCryptor/RNCryptor-Spec, vectors/v3/password), so
    /// decryption is checked against the format itself, not just `encrypt`.
    #[test]
    fn decrypts_the_spec_test_vectors() {
        let hex = |text: &str| -> Vec<u8> {
            (0..text.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
                .collect()
        };
        let vectors = [
            // Allfieldsemptyorzero(withone-bytepassword)
            (
                "a",
                "",
                "03010000000000000000000000000000000000000000000000000000000000000000b3039be31cd7ece5e754f5c8da17003666313ae8a89ddcf8e3cb41fdc130b2329dbe07d6f4d32c34e050c8bd7e933b12",
            ),
            // Onebyte
            (
                "thepassword",
                "01",
                "03010001020304050607010203040506070802030405060708090a0b0c0d0e0f0001a1f8730e0bf480eb7b70f690abf21e029514164ad3c474a51b30c7eaa1ca545b7de3de5b010acbad0a9a13857df696a8",
            ),
            // Exactlyoneblock
            (
                "thepassword",
                "0123456789abcdef",
                "030101020304050607000203040506070801030405060708090a0b0c0d0e0f0001020e437fe809309c03fd53a475131e9a1978b8eaef576f60adb8ce2320849ba32d742900438ba897d22210c76c35c849df",
            ),
            // Morethanoneblock
            (
                "thepassword",
                "0123456789abcdef01234567",
                "0301020304050607000103040506070801020405060708090a0b0c0d0e0f00010203e01bbda5df2ca8adace38f6c588d291e03f951b78d3417bc2816581dc6b767f1a2e57597512b18e1638f21235fa5928c",
            ),
            // Multibytepassword
            (
                "中文密码",
                "23456789abcdef0123456701",
                "03010304050607000102040506070801020305060708090a0b0c0d0e0f00010203048a9e08bdec1c4bfe13e81fb85f009ab3ddb91387e809c4ad86d9e8a6014557716657bd317d4bb6a7644615b3de402341",
            ),
            // Longertextandpassword
            (
                "It was the best of times, it was the worst of times; it was the age of wisdom, it was the age of foolishness;",
                "697420776173207468652065706f6368206f662062656c6965662c20697420776173207468652065706f6368206f6620696e63726564756c6974793b206974207761732074686520736561736f6e206f66204c696768742c206974207761732074686520736561736f6e206f66204461726b6e6573733b206974207761732074686520737072696e67206f6620686f70652c20697420776173207468652077696e746572206f6620646573706169723b207765206861642065766572797468696e67206265666f72652075732c20776520686164206e6f7468696e67206265666f72652075733b207765207765726520616c6c20676f696e67206469726563746c7920746f2048656176656e2c207765207765726520616c6c20676f696e6720746865206f74686572207761792e0a0a",
                "030104050607000102030506070801020304060708090a0b0c0d0e0f000102030405d564c7a99da921a6e7c4078a82641d95479551283167a2c81f31ab80c9d7d8beb770111decd3e3d29bbdf7ebbfc5f10ac87e7e55bfb5a7f487bcd39835705e83b9c049c6d6952be011f8ddb1a14fc0c925738de017e62b1d621ccdb75f2937d0a1a70e44d843b9c61037dee2998b2bbd740b910232eea71961168838f6995b9964173b34c0bcd311a2c87e271630928bae301a8f4703ac2ae4699f3c285abf1c55ac324b073a958ae52ee8c3bd68f919c09eb1cd28142a1996a9e6cbff5f4f4e1dba07d29ff66860db9895a48233140ca249419d63046448db1b0f4252a6e4edb947fd0071d1e52bc15600622fa548a6773963618150797a8a80e592446df5926d0bfd32b544b796f3359567394f77e7b171b2f9bc5f2caf7a0fac0da7d04d6a86744d6e06d02fbe15d0f580a1d5bd16ad91348003611358dcb4ac9990955f6cbbbfb185941d4b4b71ce7f9ba6efc1270b7808838b6c7b7ef17e8db919b34fac",
            ),
        ];
        for (password, plain, cipher) in vectors {
            assert_eq!(
                decrypt(&hex(cipher), password).unwrap(),
                hex(plain),
                "{plain}"
            );
        }
    }
}
