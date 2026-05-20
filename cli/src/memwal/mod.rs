// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! MemWal relayer HTTP client.
//!
//! MemWal stores encrypted memory blobs on Walrus and indexes them with
//! vector embeddings on Sui. We use it as the durable, decentralised home
//! for reasoning traces, with one MemWal namespace per walgit repo.
//!
//! Auth model: every `/api/*` request carries three headers signed by a
//! delegate key registered on the user's `MemWalAccount`:
//!
//! - `x-public-key`  — hex Ed25519 public key (32 bytes)
//! - `x-signature`   — hex Ed25519 signature (64 bytes) over
//!                     `"{timestamp}.{method}.{path}.{body_sha256}"`
//! - `x-timestamp`   — unix seconds; the relayer enforces a 5-minute window
//!
//! The relayer resolves the owning account by looking up the public key in
//! `MemWalAccount.delegate_keys` on Sui, so the caller doesn't pass the
//! account ID on every request — it's part of the keypair's identity.
//!
//! Two write modes:
//!
//! - [`MemWalClient::remember`] (regular) — POST `text + namespace`; relayer
//!   embeds and Seal-encrypts. Privacy tradeoff: plaintext travels to the
//!   relayer.
//! - [`MemWalClient::remember_manual`] (manual) — client does embedding +
//!   Seal-encryption locally, relayer only uploads the bytes.

pub mod auth;

use crate::error::{Result, WalGitError};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub struct MemWalClient {
    base_url: String,
    delegate_key: ed25519_dalek::SigningKey,
    /// Sui object ID of the `MemWalAccount` we delegate from. The relayer
    /// doesn't need this on the wire — it derives the owner from the key —
    /// but we keep it for logging and `walgit config --show`.
    pub account_id: String,
    http: Client,
}

impl MemWalClient {
    pub fn new(base_url: String, account_id: String, delegate_key_bytes: [u8; 32]) -> Self {
        let delegate_key = ed25519_dalek::SigningKey::from_bytes(&delegate_key_bytes);
        Self {
            // Trim trailing slash so we can blindly append `/api/...`.
            base_url: base_url.trim_end_matches('/').to_string(),
            delegate_key,
            account_id,
            http: Client::new(),
        }
    }

    /// Public key of the delegate, hex-encoded — useful when registering this
    /// key on `MemWalAccount.delegate_keys` or for debugging.
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.delegate_key.verifying_key().to_bytes())
    }

    /// Unauthenticated health probe. Verifies the relayer is reachable
    /// before doing real work (e.g., during `walgit config --show`).
    pub async fn health(&self) -> Result<HealthResponse> {
        let url = format!("{}/health", self.base_url);
        let resp = self.http.get(&url).send().await.map_err(http_err)?;
        let status = resp.status();
        let body = resp.text().await.map_err(http_err)?;
        if !status.is_success() {
            return Err(WalGitError::other(format!(
                "MemWal /health returned {}: {}",
                status, body
            )));
        }
        serde_json::from_str(&body).map_err(|e| {
            WalGitError::other(format!("MemWal /health: malformed JSON ({}): {}", e, body))
        })
    }

    /// Submit a plaintext memory for the relayer to embed, encrypt, and
    /// store on Walrus. Returns `202 Accepted` immediately with a job_id; the
    /// caller can poll job status if needed (we don't wait by default —
    /// trace upload is fire-and-forget per push).
    pub async fn remember(&self, text: &str, namespace: Option<&str>) -> Result<RememberAccepted> {
        let body = RememberRequest {
            text: text.to_string(),
            namespace: namespace.map(str::to_string),
        };
        self.post_signed("/api/remember", &body).await
    }

    /// Semantic search over a namespace. Returns plaintext matches (the
    /// relayer decrypts before responding). Used by `walgit trace recall
    /// <query>` to surface past decisions relevant to current work.
    pub async fn recall(
        &self,
        query: &str,
        limit: Option<u32>,
        namespace: Option<&str>,
    ) -> Result<RecallResponse> {
        let body = RecallRequest {
            query: query.to_string(),
            limit,
            namespace: namespace.map(str::to_string),
        };
        self.post_signed("/api/recall", &body).await
    }

    /// POST JSON to a path with the relayer's Ed25519 auth headers attached.
    /// Generic over response type since several endpoints share this shape.
    async fn post_signed<TReq, TResp>(&self, path: &str, body: &TReq) -> Result<TResp>
    where
        TReq: Serialize,
        TResp: serde::de::DeserializeOwned,
    {
        let body_bytes = serde_json::to_vec(body)?;
        let body_sha256 = hex::encode(Sha256::digest(&body_bytes));
        let ts = chrono::Utc::now().timestamp().to_string();
        // Fresh UUID v4 per request; the relayer rejects re-use within a
        // 10-minute window via Redis-tracked nonces.
        let nonce = uuid::Uuid::new_v4().to_string();

        let signed = auth::sign_request(
            &self.delegate_key,
            &ts,
            "POST",
            path,
            &body_sha256,
            &nonce,
            &self.account_id,
        );

        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .header("x-public-key", signed.public_key_hex)
            .header("x-signature", signed.signature_hex)
            .header("x-timestamp", ts)
            .header("x-nonce", nonce)
            .header("x-account-id", &self.account_id)
            .body(body_bytes)
            .send()
            .await
            .map_err(http_err)?;

        let status = resp.status();
        let text = resp.text().await.map_err(http_err)?;
        if !status.is_success() {
            return Err(WalGitError::other(format!(
                "MemWal POST {} returned {}: {}",
                path, status, text
            )));
        }
        serde_json::from_str(&text).map_err(|e| {
            WalGitError::other(format!(
                "MemWal POST {}: malformed JSON ({}): {}",
                path, e, text
            ))
        })
    }
}

fn http_err(e: reqwest::Error) -> WalGitError {
    WalGitError::other(format!("MemWal HTTP error: {}", e))
}

#[derive(Debug, Serialize)]
struct RememberRequest {
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
}

/// `POST /api/remember` returns 202 with the queued job's identifier.
#[derive(Debug, Deserialize, Clone)]
pub struct RememberAccepted {
    #[serde(default)]
    pub job_id: Option<String>,
    /// Some relayer builds return the eventual blob_id synchronously when
    /// upload is fast; capture it opportunistically.
    #[serde(default)]
    pub blob_id: Option<String>,
    /// Free-form passthrough of any other fields the relayer chooses to
    /// include (status, eta, etc.). We don't depend on these but logging
    /// them helps debug surprises.
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HealthResponse {
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct RecallRequest {
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
}

/// Response from `POST /api/recall`. The relayer returns
/// `{results: [...], total: N, dropped_count: M}`. We accept either
/// `results` or `matches` so a doc-driven rename doesn't break us.
///
/// `dropped_count` is the number of below-threshold matches the relayer
/// filtered out — useful to surface to the user as "memory exists but
/// wasn't semantically close enough."
#[derive(Debug, Deserialize, Clone)]
pub struct RecallResponse {
    #[serde(default, alias = "matches")]
    pub results: Vec<RecallMatch>,
    #[serde(default)]
    pub total: Option<u32>,
    #[serde(default)]
    pub dropped_count: Option<u32>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RecallMatch {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub distance: Option<f32>,
    #[serde(default)]
    pub score: Option<f32>,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PRIVKEY_HEX: &str =
        "95e0c3184caac9377b2b5f958d41c2e3fea636f837d6e2b72fdc674404af7a1b";
    const TEST_PUBKEY_HEX: &str =
        "5b067f97395e1f310ba8fe2950da8320eacc046a17db8f7cfd536b7e3ca56bc5";

    #[test]
    fn public_key_matches_delegate_pair() {
        // Sanity-check the well-known testnet pair the user gave us so we
        // notice immediately if ed25519-dalek's API changes shape.
        let priv_bytes: [u8; 32] = hex::decode(TEST_PRIVKEY_HEX).unwrap().try_into().unwrap();
        let c = MemWalClient::new("http://example.invalid".into(), "0xtest".into(), priv_bytes);
        assert_eq!(c.public_key_hex(), TEST_PUBKEY_HEX);
    }
}
