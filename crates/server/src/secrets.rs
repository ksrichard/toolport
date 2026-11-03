//! Encryption of upstream API secrets at rest.
//!
//! Sealed format: version byte (1), 12-byte random nonce, then AES-256-GCM ciphertext with tag.
//! The API id is the associated data, so a sealed value cannot be moved to another API.

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use zeroize::Zeroizing;

const VERSION: u8 = 1;
const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SecretsError {
    #[error("the key must have 64 hex characters")]
    BadKey,
    #[error("unknown sealed secret version {0}")]
    UnknownVersion(u8),
    #[error("the sealed secret is malformed")]
    Malformed,
    #[error("cannot decrypt the sealed secret")]
    Decrypt,
}

/// The AES key. It has no `Debug`, so a log line cannot print it.
pub struct SecretsKey(Zeroizing<[u8; 32]>);

impl SecretsKey {
    pub fn from_hex(hex: &str) -> Result<Self, SecretsError> {
        let bytes = hex.as_bytes();
        if bytes.len() != 64 {
            return Err(SecretsError::BadKey);
        }
        let mut key = Zeroizing::new([0u8; 32]);
        let (pairs, _) = bytes.as_chunks::<2>();
        for (out, [high, low]) in key.iter_mut().zip(pairs) {
            *out = (nibble(*high)? << 4) | nibble(*low)?;
        }
        Ok(Self(key))
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new(&(*self.0).into())
    }
}

fn nibble(byte: u8) -> Result<u8, SecretsError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(SecretsError::BadKey),
    }
}

/// A decrypted secret. It has no `Debug`, `Display`, or `Serialize`, and it wipes its memory on drop.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

pub fn seal(key: &SecretsKey, api_id: i64, plain: &str) -> Result<Vec<u8>, SecretsError> {
    let mut nonce = [0u8; NONCE_LEN];
    rand::fill(nonce.as_mut_slice());
    let aad = api_id.to_be_bytes();
    let ciphertext = key
        .cipher()
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: plain.as_bytes(),
                aad: &aad,
            },
        )
        .map_err(|_| SecretsError::Decrypt)?;
    let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    sealed.push(VERSION);
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
}

pub fn open(key: &SecretsKey, api_id: i64, sealed: &[u8]) -> Result<Secret, SecretsError> {
    let (&version, rest) = sealed.split_first().ok_or(SecretsError::Malformed)?;
    if version != VERSION {
        return Err(SecretsError::UnknownVersion(version));
    }
    if rest.len() <= NONCE_LEN {
        return Err(SecretsError::Malformed);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| SecretsError::Malformed)?;
    let aad = api_id.to_be_bytes();
    let plain = key
        .cipher()
        .decrypt(
            &Nonce::from(nonce),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| SecretsError::Decrypt)?;
    let text = String::from_utf8(plain).map_err(|_| SecretsError::Decrypt)?;
    Ok(Secret(Zeroizing::new(text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SecretsKey {
        SecretsKey::from_hex(&"0".repeat(64)).unwrap()
    }

    #[test]
    fn seal_then_open_gives_the_same_text() {
        let sealed = seal(&key(), 7, "abc").unwrap();
        assert_eq!(open(&key(), 7, &sealed).unwrap().expose(), "abc");
    }

    #[test]
    fn two_seals_of_one_text_differ() {
        assert_ne!(
            seal(&key(), 7, "abc").unwrap(),
            seal(&key(), 7, "abc").unwrap()
        );
    }

    #[test]
    fn open_with_another_api_id_fails() {
        let sealed = seal(&key(), 7, "abc").unwrap();
        assert!(matches!(
            open(&key(), 8, &sealed),
            Err(SecretsError::Decrypt)
        ));
    }

    #[test]
    fn open_with_a_changed_byte_fails() {
        let mut sealed = seal(&key(), 7, "abc").unwrap();
        *sealed.last_mut().unwrap() ^= 1;
        assert!(matches!(
            open(&key(), 7, &sealed),
            Err(SecretsError::Decrypt)
        ));
    }

    #[test]
    fn open_with_an_unknown_version_fails() {
        let mut sealed = seal(&key(), 7, "abc").unwrap();
        sealed[0] = 2;
        assert!(matches!(
            open(&key(), 7, &sealed),
            Err(SecretsError::UnknownVersion(2))
        ));
    }

    #[test]
    fn key_must_be_64_hex_characters() {
        assert!(matches!(
            SecretsKey::from_hex("abc"),
            Err(SecretsError::BadKey)
        ));
        assert!(matches!(
            SecretsKey::from_hex(&"g".repeat(64)),
            Err(SecretsError::BadKey)
        ));
    }
}
