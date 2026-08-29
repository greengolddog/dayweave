use std::{collections::BTreeMap, sync::Arc};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use thiserror::Error;
use zeroize::Zeroize;

use crate::config::CredentialKey;

const MAGIC: &[u8; 4] = b"DWG1";
const HEADER_LENGTH: usize = 4 + 4 + 12;

#[derive(Clone)]
pub(crate) struct SecretCipher {
    keys: Arc<BTreeMap<u32, CredentialKey>>,
    active_version: u32,
}

impl std::fmt::Debug for SecretCipher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretCipher")
            .field("key_versions", &self.keys.keys().collect::<Vec<_>>())
            .field("active_version", &self.active_version)
            .finish()
    }
}

impl SecretCipher {
    #[must_use]
    pub(crate) fn new(keys: Arc<BTreeMap<u32, CredentialKey>>, active_version: u32) -> Self {
        Self {
            keys,
            active_version,
        }
    }

    /// Seals a secret with the active AES-256-GCM key and caller-supplied AAD.
    /// The returned envelope redundantly carries its key version so a database
    /// column/envelope mismatch fails rather than silently trying another key.
    pub(crate) fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<SealedSecret, CryptoError> {
        let key = self
            .keys
            .get(&self.active_version)
            .ok_or(CryptoError::UnknownKeyVersion)?;
        let cipher =
            Aes256Gcm::new_from_slice(key.expose()).map_err(|_| CryptoError::InvalidKey)?;
        let mut nonce_bytes = [0_u8; 12];
        getrandom::fill(&mut nonce_bytes).map_err(|_| CryptoError::Randomness)?;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| CryptoError::Encryption)?;
        let mut envelope = Vec::with_capacity(HEADER_LENGTH + ciphertext.len());
        envelope.extend_from_slice(MAGIC);
        envelope.extend_from_slice(&self.active_version.to_be_bytes());
        envelope.extend_from_slice(&nonce_bytes);
        envelope.extend_from_slice(&ciphertext);
        Ok(SealedSecret {
            key_version: self.active_version,
            ciphertext: envelope,
        })
    }

    pub(crate) fn open(
        &self,
        expected_key_version: u32,
        envelope: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        if envelope.len() <= HEADER_LENGTH || &envelope[..4] != MAGIC {
            return Err(CryptoError::InvalidEnvelope);
        }
        let embedded_version = u32::from_be_bytes(
            envelope[4..8]
                .try_into()
                .map_err(|_| CryptoError::InvalidEnvelope)?,
        );
        if embedded_version != expected_key_version {
            return Err(CryptoError::KeyVersionMismatch);
        }
        let key = self
            .keys
            .get(&embedded_version)
            .ok_or(CryptoError::UnknownKeyVersion)?;
        let cipher =
            Aes256Gcm::new_from_slice(key.expose()).map_err(|_| CryptoError::InvalidKey)?;
        cipher
            .decrypt(
                Nonce::from_slice(&envelope[8..20]),
                Payload {
                    msg: &envelope[HEADER_LENGTH..],
                    aad,
                },
            )
            .map_err(|_| CryptoError::Authentication)
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct SealedSecret {
    pub key_version: u32,
    pub ciphertext: Vec<u8>,
}

impl std::fmt::Debug for SealedSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SealedSecret")
            .field("key_version", &self.key_version)
            .field("ciphertext", &"[REDACTED]")
            .finish()
    }
}

impl Drop for SealedSecret {
    fn drop(&mut self) {
        self.ciphertext.zeroize();
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CryptoError {
    #[error("credential key is invalid")]
    InvalidKey,
    #[error("credential encryption randomness failed")]
    Randomness,
    #[error("credential encryption failed")]
    Encryption,
    #[error("encrypted credential envelope is invalid")]
    InvalidEnvelope,
    #[error("credential key version does not match its envelope")]
    KeyVersionMismatch,
    #[error("credential key version is not configured")]
    UnknownKeyVersion,
    #[error("encrypted credential authentication failed")]
    Authentication,
}

pub(crate) fn erase(bytes: &mut Vec<u8>) {
    bytes.zeroize();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google_oauth::sync_cursor_aad;
    use uuid::Uuid;

    fn cipher() -> SecretCipher {
        SecretCipher::new(
            Arc::new(BTreeMap::from([
                (1, CredentialKey::from_test_bytes([7; 32])),
                (2, CredentialKey::from_test_bytes([8; 32])),
            ])),
            2,
        )
    }

    #[test]
    fn envelope_authenticates_aad_and_version() {
        let cipher = cipher();
        let sealed = cipher.seal(b"secret", b"row-a").expect("seal");
        assert_eq!(sealed.key_version, 2);
        assert_eq!(
            cipher
                .open(sealed.key_version, &sealed.ciphertext, b"row-a")
                .expect("open"),
            b"secret"
        );
        assert_eq!(
            cipher.open(1, &sealed.ciphertext, b"row-a"),
            Err(CryptoError::KeyVersionMismatch)
        );
        assert_eq!(
            cipher.open(2, &sealed.ciphertext, b"row-b"),
            Err(CryptoError::Authentication)
        );

        let old_cipher = SecretCipher::new(cipher.keys.clone(), 1);
        let old = old_cipher.seal(b"old-secret", b"row-a").expect("old seal");
        assert_eq!(
            cipher
                .open(old.key_version, &old.ciphertext, b"row-a")
                .expect("configured old key remains decryptable"),
            b"old-secret"
        );
        let mut unknown = sealed.ciphertext.clone();
        unknown[4..8].copy_from_slice(&3_u32.to_be_bytes());
        assert_eq!(
            cipher.open(3, &unknown, b"row-a"),
            Err(CryptoError::UnknownKeyVersion)
        );

        let workspace_id = Uuid::from_u128(10);
        let user_id = Uuid::from_u128(11);
        let account_id = Uuid::from_u128(12);
        let cursor_aad = sync_cursor_aad(workspace_id, user_id, account_id, "calendar:primary");
        let cursor = cipher
            .seal(b"opaque-cursor", &cursor_aad)
            .expect("cursor seal");
        assert_eq!(
            cipher.open(
                cursor.key_version,
                &cursor.ciphertext,
                &sync_cursor_aad(workspace_id, user_id, account_id, "tasks:default")
            ),
            Err(CryptoError::Authentication)
        );
    }
}
