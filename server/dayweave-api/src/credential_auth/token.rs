use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::Zeroize;

const TOKEN_RANDOM_BYTES: usize = 32;
const TOKEN_PAYLOAD_LENGTH: usize = 43;
const HASH_DOMAIN: &[u8] = b"dayweave/credential/v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialKind {
    DeviceAccess,
    DeviceRefresh,
    McpClient,
    Enrollment,
    AccountRecovery,
}

/// A newly generated credential whose plaintext is erased when dropped.
///
/// It intentionally has no `Clone`, `Serialize`, or plaintext-bearing `Debug`
/// implementation. HTTP issuance code may expose it exactly once in a
/// no-store response before allowing this value to drop.
pub struct GeneratedCredential {
    kind: CredentialKind,
    raw: String,
}

impl fmt::Debug for GeneratedCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeneratedCredential")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Drop for GeneratedCredential {
    fn drop(&mut self) {
        self.raw.zeroize();
    }
}

impl GeneratedCredential {
    /// Generates 256 bits with the operating system CSPRNG.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the operating system RNG is unavailable.
    pub fn generate(kind: CredentialKind) -> Result<Self, TokenGenerationError> {
        let mut random = [0_u8; TOKEN_RANDOM_BYTES];
        if getrandom::fill(&mut random).is_err() {
            random.zeroize();
            return Err(TokenGenerationError::Unavailable);
        }
        let mut raw = String::with_capacity(kind.prefix().len() + TOKEN_PAYLOAD_LENGTH);
        raw.push_str(kind.prefix());
        raw.push_str(&URL_SAFE_NO_PAD.encode(random));
        random.zeroize();
        Ok(Self { kind, raw })
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.raw
    }

    /// Revalidates the generated value as a borrowed persistence credential.
    ///
    /// # Errors
    ///
    /// Returns a redacted error if the generator/parser invariant is violated.
    pub fn parsed(&self) -> Result<OpaqueCredential<'_>, TokenParseError> {
        OpaqueCredential::parse(self.kind, &self.raw)
    }
}

impl CredentialKind {
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::DeviceAccess => "dw_da1_",
            Self::DeviceRefresh => "dw_dr1_",
            Self::McpClient => "dw_mc1_",
            Self::Enrollment => "dw_en1_",
            Self::AccountRecovery => "dw_rc1_",
        }
    }

    const fn hash_context(self) -> &'static [u8] {
        match self {
            Self::DeviceAccess => b"device-access",
            Self::DeviceRefresh => b"device-refresh",
            Self::McpClient => b"mcp-client",
            Self::Enrollment => b"enrollment",
            Self::AccountRecovery => b"account-recovery",
        }
    }
}

/// A validated, borrowed opaque credential.
///
/// The type intentionally does not implement `Clone`, `Serialize`, or a debug
/// representation containing the credential. Callers retain ownership of the
/// source buffer and should keep it in their platform credential store.
pub struct OpaqueCredential<'a> {
    kind: CredentialKind,
    raw: &'a str,
}

impl fmt::Debug for OpaqueCredential<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpaqueCredential")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl<'a> OpaqueCredential<'a> {
    /// Validates an exact versioned prefix followed by 32 bytes encoded as
    /// unpadded URL-safe base64.
    ///
    /// # Errors
    ///
    /// Returns a single redacted error for every malformed credential.
    pub fn parse(kind: CredentialKind, raw: &'a str) -> Result<Self, TokenParseError> {
        let Some(payload) = raw.strip_prefix(kind.prefix()) else {
            return Err(TokenParseError::InvalidCredential);
        };
        if payload.len() != TOKEN_PAYLOAD_LENGTH || !payload.is_ascii() {
            return Err(TokenParseError::InvalidCredential);
        }

        let mut decoded = [0_u8; TOKEN_RANDOM_BYTES];
        let result = URL_SAFE_NO_PAD.decode_slice(payload, &mut decoded);
        let valid = matches!(result, Ok(TOKEN_RANDOM_BYTES));
        decoded.zeroize();
        if !valid {
            return Err(TokenParseError::InvalidCredential);
        }

        Ok(Self { kind, raw })
    }

    #[must_use]
    pub const fn kind(&self) -> CredentialKind {
        self.kind
    }

    pub(crate) fn persistence_digest(&self) -> [u8; 32] {
        self.persistence_digest_for(self.kind)
    }

    pub(crate) fn persistence_digest_for(&self, kind: CredentialKind) -> [u8; 32] {
        let payload = &self.raw[self.kind.prefix().len()..];
        let mut decoded = [0_u8; TOKEN_RANDOM_BYTES];
        // Parsing established the exact decoded length. Keep this branch
        // fail-closed in case the invariant is ever weakened by a refactor.
        if !matches!(
            URL_SAFE_NO_PAD.decode_slice(payload, &mut decoded),
            Ok(TOKEN_RANDOM_BYTES)
        ) {
            decoded.zeroize();
            return [0_u8; 32];
        }
        let mut hasher = Sha256::new();
        hasher.update(HASH_DOMAIN);
        hasher.update(kind.hash_context());
        hasher.update([0]);
        hasher.update(decoded);
        decoded.zeroize();
        hasher.finalize().into()
    }

    pub(crate) fn has_same_secret_material(&self, other: &Self) -> Result<bool, TokenParseError> {
        let mut left = [0_u8; TOKEN_RANDOM_BYTES];
        let mut right = [0_u8; TOKEN_RANDOM_BYTES];
        let left_valid = matches!(
            URL_SAFE_NO_PAD.decode_slice(&self.raw[self.kind.prefix().len()..], &mut left,),
            Ok(TOKEN_RANDOM_BYTES)
        );
        let right_valid = matches!(
            URL_SAFE_NO_PAD.decode_slice(&other.raw[other.kind.prefix().len()..], &mut right,),
            Ok(TOKEN_RANDOM_BYTES)
        );
        let same = left.ct_eq(&right);
        left.zeroize();
        right.zeroize();
        if !left_valid || !right_valid {
            return Err(TokenParseError::InvalidCredential);
        }
        Ok(bool::from(same))
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum TokenParseError {
    #[error("invalid credential")]
    InvalidCredential,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum TokenGenerationError {
    #[error("credential generation failed")]
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(kind: CredentialKind, marker: u8) -> String {
        format!("{}{}", kind.prefix(), URL_SAFE_NO_PAD.encode([marker; 32]))
    }

    #[test]
    fn accepts_only_exact_versioned_32_byte_tokens() {
        let raw = token(CredentialKind::DeviceAccess, 7);
        let parsed = OpaqueCredential::parse(CredentialKind::DeviceAccess, &raw)
            .expect("valid synthetic token");
        assert_eq!(parsed.kind(), CredentialKind::DeviceAccess);
        assert!(!format!("{parsed:?}").contains(&raw));

        assert!(
            OpaqueCredential::parse(CredentialKind::DeviceRefresh, &raw).is_err(),
            "credential kinds are not interchangeable"
        );
        assert!(OpaqueCredential::parse(CredentialKind::DeviceAccess, &format!("{raw}=")).is_err());
        assert!(
            OpaqueCredential::parse(
                CredentialKind::DeviceAccess,
                &format!(
                    "{}{}",
                    CredentialKind::DeviceAccess.prefix(),
                    URL_SAFE_NO_PAD.encode([1_u8; 31])
                )
            )
            .is_err()
        );
        assert!(
            OpaqueCredential::parse(
                CredentialKind::DeviceAccess,
                &format!(
                    "{}{}",
                    CredentialKind::DeviceAccess.prefix(),
                    "!".repeat(43)
                )
            )
            .is_err()
        );
    }

    #[test]
    fn persistence_hashes_are_stable_and_domain_separated() {
        let access_raw = token(CredentialKind::DeviceAccess, 11);
        let refresh_raw = token(CredentialKind::DeviceRefresh, 11);
        let recovery_raw = token(CredentialKind::AccountRecovery, 11);
        let access = OpaqueCredential::parse(CredentialKind::DeviceAccess, &access_raw).unwrap();
        let access_again =
            OpaqueCredential::parse(CredentialKind::DeviceAccess, &access_raw).unwrap();
        let refresh = OpaqueCredential::parse(CredentialKind::DeviceRefresh, &refresh_raw).unwrap();
        let recovery =
            OpaqueCredential::parse(CredentialKind::AccountRecovery, &recovery_raw).unwrap();

        assert_eq!(
            access.persistence_digest(),
            access_again.persistence_digest()
        );
        assert_ne!(access.persistence_digest(), refresh.persistence_digest());
        assert_ne!(access.persistence_digest(), recovery.persistence_digest());
        assert_ne!(refresh.persistence_digest(), recovery.persistence_digest());
        assert_ne!(
            access.persistence_digest().as_slice(),
            access_raw.as_bytes()
        );
    }

    #[test]
    fn compares_underlying_material_across_public_prefixes_without_exposing_it() {
        let access_raw = token(CredentialKind::DeviceAccess, 21);
        let matching_refresh_raw = token(CredentialKind::DeviceRefresh, 21);
        let different_refresh_raw = token(CredentialKind::DeviceRefresh, 22);
        let matching_recovery_raw = token(CredentialKind::AccountRecovery, 21);
        let access = OpaqueCredential::parse(CredentialKind::DeviceAccess, &access_raw).unwrap();
        let matching_refresh =
            OpaqueCredential::parse(CredentialKind::DeviceRefresh, &matching_refresh_raw).unwrap();
        let different_refresh =
            OpaqueCredential::parse(CredentialKind::DeviceRefresh, &different_refresh_raw).unwrap();
        let matching_recovery =
            OpaqueCredential::parse(CredentialKind::AccountRecovery, &matching_recovery_raw)
                .unwrap();

        assert!(access.has_same_secret_material(&matching_refresh).unwrap());
        assert!(access.has_same_secret_material(&matching_recovery).unwrap());
        assert!(!access.has_same_secret_material(&different_refresh).unwrap());
        assert_ne!(
            access.persistence_digest(),
            matching_refresh.persistence_digest(),
            "persistence hashes remain kind-domain-separated"
        );
        let debug = format!("{access:?} {matching_refresh:?}");
        assert!(!debug.contains(&access_raw));
        assert!(!debug.contains(&matching_refresh_raw));
        assert!(!format!("{matching_recovery:?}").contains(&matching_recovery_raw));
    }

    #[test]
    fn generates_exact_redacted_credentials() {
        let generated = GeneratedCredential::generate(CredentialKind::Enrollment)
            .expect("operating system CSPRNG");
        let raw = generated.expose().to_owned();
        assert_eq!(raw.len(), CredentialKind::Enrollment.prefix().len() + 43);
        assert!(generated.parsed().is_ok());
        assert!(!format!("{generated:?}").contains(&raw));

        let other = GeneratedCredential::generate(CredentialKind::Enrollment)
            .expect("operating system CSPRNG");
        assert_ne!(generated.expose(), other.expose());
    }
}
