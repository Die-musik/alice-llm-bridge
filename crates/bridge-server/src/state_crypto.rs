use chacha20poly1305::{
    ChaCha20Poly1305, Key, Nonce,
    aead::{Aead, KeyInit},
};
use hmac::{Hmac, Mac};
use sha2::Sha256;

const NONCE_BYTES: usize = 12;
const KEY_BYTES: usize = 32;
const CURRENT_KEY_VERSION: i16 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedPayload {
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub key_version: i16,
}

#[derive(Clone)]
pub struct StateCipher {
    key: [u8; KEY_BYTES],
    key_version: i16,
}

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("state encryption key must be exactly 64 hexadecimal characters")]
    InvalidKey,
    #[error("encrypted state has an unsupported key version")]
    UnsupportedKeyVersion,
    #[error("encrypted state has an invalid nonce")]
    InvalidNonce,
    #[error("encrypted state authentication failed")]
    Authentication,
}

impl StateCipher {
    pub fn from_hex(value: &str) -> Result<Self, CryptoError> {
        if value.len() != KEY_BYTES * 2 {
            return Err(CryptoError::InvalidKey);
        }

        let mut key = [0_u8; KEY_BYTES];
        hex::decode_to_slice(value, &mut key).map_err(|_| CryptoError::InvalidKey)?;
        Ok(Self {
            key,
            key_version: CURRENT_KEY_VERSION,
        })
    }

    pub fn seal(&self, plaintext: &[u8]) -> Result<EncryptedPayload, CryptoError> {
        let nonce_bytes: [u8; NONCE_BYTES] = rand::random();
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key));
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
            .map_err(|_| CryptoError::Authentication)?;

        Ok(EncryptedPayload {
            nonce: nonce_bytes.to_vec(),
            ciphertext,
            key_version: self.key_version,
        })
    }

    pub fn open(&self, payload: &EncryptedPayload) -> Result<Vec<u8>, CryptoError> {
        if payload.key_version != self.key_version {
            return Err(CryptoError::UnsupportedKeyVersion);
        }
        let nonce: [u8; NONCE_BYTES] = payload
            .nonce
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::InvalidNonce)?;
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&self.key));
        cipher
            .decrypt(Nonce::from_slice(&nonce), payload.ciphertext.as_ref())
            .map_err(|_| CryptoError::Authentication)
    }

    pub(crate) fn pairing_code_hash(
        &self,
        user_id: &str,
        application_id: &str,
        code: &str,
    ) -> [u8; 32] {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.key)
            .expect("HMAC accepts keys of any length");
        for value in [
            user_id.as_bytes(),
            application_id.as_bytes(),
            code.as_bytes(),
        ] {
            mac.update(&[0]);
            mac.update(value);
        }
        mac.finalize().into_bytes().into()
    }

    pub(crate) fn pairing_code_matches(
        &self,
        stored_hash: &[u8],
        user_id: &str,
        application_id: &str,
        code: &str,
    ) -> bool {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.key)
            .expect("HMAC accepts keys of any length");
        for value in [
            user_id.as_bytes(),
            application_id.as_bytes(),
            code.as_bytes(),
        ] {
            mac.update(&[0]);
            mac.update(value);
        }
        mac.verify_slice(stored_hash).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::StateCipher;

    const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    #[test]
    fn roundtrip_returns_exact_utf8_bytes() {
        let cipher = StateCipher::from_hex(KEY).unwrap();
        let source = "Продолжить рассказ?".as_bytes();

        let sealed = cipher.seal(source).unwrap();

        assert_eq!(cipher.open(&sealed).unwrap(), source);
    }

    #[test]
    fn tampered_ciphertext_fails_authentication() {
        let cipher = StateCipher::from_hex(KEY).unwrap();
        let mut sealed = cipher.seal(b"private household state").unwrap();
        sealed.ciphertext[0] ^= 1;

        assert!(cipher.open(&sealed).is_err());
    }

    #[test]
    fn repeated_seals_use_unique_nonces_and_ciphertext() {
        let cipher = StateCipher::from_hex(KEY).unwrap();

        let first = cipher.seal(b"same plaintext").unwrap();
        let second = cipher.seal(b"same plaintext").unwrap();

        assert_ne!(first.nonce, second.nonce);
        assert_ne!(first.ciphertext, second.ciphertext);
    }

    #[test]
    fn key_must_be_exactly_32_bytes_of_hex() {
        assert!(StateCipher::from_hex("00").is_err());
        assert!(StateCipher::from_hex(&"z".repeat(64)).is_err());
        assert!(StateCipher::from_hex(&"00".repeat(33)).is_err());
    }
}
