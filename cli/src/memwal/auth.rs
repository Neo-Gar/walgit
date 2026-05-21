// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Ed25519 request signing for the MemWal relayer.
//!
//! Sign payload format (verified against MystenLabs/MemWal `services/server/src/auth.rs`;
//! the published docs only show the first four fields and are stale):
//!
//! ```text
//! {timestamp}.{method}.{path_and_query}.{body_sha256}.{nonce}.{account_id}
//! ```
//!
//! where:
//! - `timestamp` is unix seconds as a decimal string,
//! - `method` is the HTTP verb in uppercase (`POST`, `GET`),
//! - `path_and_query` is the request path including any query string,
//! - `body_sha256` is the lowercase hex SHA-256 of the request body
//!   (`sha256("")` for GETs with no body),
//! - `nonce` is a UUID v4 string sent as `x-nonce`; the relayer tracks
//!   used nonces in Redis for 10 minutes for replay protection,
//! - `account_id` is the Sui `MemWalAccount` ID, also sent as the
//!   `x-account-id` header.
//!
//! Missing `x-nonce` is what triggers `426 Upgrade Required` — the relayer
//! reads it as a legacy SDK without replay protection.

use ed25519_dalek::{Signer, SigningKey};

/// Output of [`sign_request`]: the three header values the relayer expects.
/// `x-timestamp` is the caller's responsibility (we don't bundle it here so
/// the caller can re-use the same timestamp it embedded in the signature).
pub struct Signed {
    pub public_key_hex: String,
    pub signature_hex: String,
}

pub fn sign_request(
    key: &SigningKey,
    timestamp: &str,
    method: &str,
    path_and_query: &str,
    body_sha256_hex: &str,
    nonce: &str,
    account_id: &str,
) -> Signed {
    let payload = format!(
        "{}.{}.{}.{}.{}.{}",
        timestamp, method, path_and_query, body_sha256_hex, nonce, account_id
    );
    let signature = key.sign(payload.as_bytes());
    Signed {
        public_key_hex: hex::encode(key.verifying_key().to_bytes()),
        signature_hex: hex::encode(signature.to_bytes()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Verifier, VerifyingKey};

    #[test]
    fn signed_payload_verifies() {
        // Use an ephemeral key — no private key material hardcoded in source.
        let mut rng = rand::thread_rng();
        let key = SigningKey::generate(&mut rng);

        let ts = "1700000000";
        let nonce = "550e8400-e29b-41d4-a716-446655440000";
        let acct = "0xabc";
        let signed = sign_request(&key, ts, "POST", "/api/remember", "deadbeef", nonce, acct);

        let payload = format!(
            "{}.{}.{}.{}.{}.{}",
            ts, "POST", "/api/remember", "deadbeef", nonce, acct
        );
        let pub_bytes: [u8; 32] = hex::decode(&signed.public_key_hex)
            .unwrap()
            .try_into()
            .unwrap();
        let vk = VerifyingKey::from_bytes(&pub_bytes).unwrap();
        let sig_bytes: [u8; 64] = hex::decode(&signed.signature_hex)
            .unwrap()
            .try_into()
            .unwrap();
        let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
        vk.verify(payload.as_bytes(), &sig).unwrap();
    }
}
