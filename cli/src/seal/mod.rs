// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Seal IBE encrypt/decrypt for private repositories.
//!
//! Encrypt (push): packfile → seal_encrypt with key server IBE pubkey → BCS bytes on Walrus.
//! Decrypt (fetch): EncryptedObject ← seal key server fetch_key with PTB proof.

use crate::error::{Result, WalGitError};
use crate::sui::keystore::{KeyPair, load_keypair};
use crate::sui::types::seal_id;
use base64::Engine as _;
use blake2::{Blake2b, Digest as _, digest::consts::U32};
use ed25519_dalek::Signer;
use fastcrypto::ed25519::{Ed25519KeyPair, Ed25519Signature};
use fastcrypto::groups::bls12381::{G2_ELEMENT_BYTE_LENGTH, G2Element};
use fastcrypto::serde_helpers::ToFromByteArray;
use fastcrypto::traits::{KeyPair as _, Signer as _};
use seal_crypto::IBEPublicKeys;
use seal_sdk::{
    Certificate, EncryptedObject, FetchKeyRequest, FetchKeyResponse, IBEPublicKey, genkey,
    seal_encrypt, signed_message, signed_request, decrypt_seal_responses,
};
use serde_json::json;
use std::collections::HashMap;
use sui_sdk_types::{
    Address, Argument, Command, Ed25519PublicKey as SuiEd25519PublicKey,
    Ed25519Signature as SuiEd25519Signature, Identifier, Input, MoveCall,
    ProgrammableTransaction, SharedInput, SimpleSignature, UserSignature,
};

pub struct SealClient {
    graphql_url: String,
    key_server_id: String,
    key_server_url: String,
}

impl SealClient {
    pub fn new(graphql_url: String, key_server_id: String, key_server_url: String) -> Self {
        Self {
            graphql_url,
            key_server_id,
            key_server_url,
        }
    }

    /// Encrypt `data` using Seal IBE. Returns BCS-serialized `EncryptedObject` bytes.
    pub async fn encrypt(&self, package_id: &str, repo_id: &str, data: &[u8]) -> Result<Vec<u8>> {
        let server_pk = fetch_ibe_public_key(&self.graphql_url, &self.key_server_id).await?;
        let server_obj_id = parse_address(&self.key_server_id)?;
        let pkg_obj_id = parse_address(package_id)?;
        let identity = seal_id(package_id, repo_id);

        let public_keys = IBEPublicKeys::BonehFranklinBLS12381(vec![server_pk]);

        let (encrypted_obj, _) = seal_encrypt(
            pkg_obj_id,
            identity,
            vec![server_obj_id],
            &public_keys,
            1,
            seal_crypto::EncryptionInput::Aes256Gcm {
                data: data.to_vec(),
                aad: None,
            },
        )
        .map_err(|e| WalGitError::SealEncrypt(format!("{:?}", e)))?;

        bcs::to_bytes(&encrypted_obj).map_err(WalGitError::from)
    }

    /// Decrypt `encrypted_data` (BCS-encoded `EncryptedObject`) using Seal key servers.
    pub async fn decrypt(
        &self,
        package_id: &str,
        repo_id: &str,
        acl_id: &str,
        acl_initial_version: u64,
        active_address: &str,
        wallet_path: Option<&str>,
        encrypted_data: &[u8],
    ) -> Result<Vec<u8>> {
        let encrypted_obj: EncryptedObject = bcs::from_bytes(encrypted_data)
            .map_err(|e| WalGitError::SealDecrypt(format!("bcs decode: {}", e)))?;

        let server_pk = fetch_ibe_public_key(&self.graphql_url, &self.key_server_id).await?;
        let server_obj_id = parse_address(&self.key_server_id)?;
        let mut server_pk_map = HashMap::new();
        server_pk_map.insert(server_obj_id, server_pk);

        // Ephemeral ElGamal keypair for key wrapping.
        let mut rng = rand::thread_rng();
        let (enc_sk, enc_pk, enc_vk) = genkey(&mut rng);

        // Session keypair: ephemeral Ed25519 signed by user's wallet key.
        let session_kp = Ed25519KeyPair::generate(&mut rng);
        let session_pk = session_kp.public().clone();

        let creation_time_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let msg = signed_message(package_id.to_string(), &session_pk, creation_time_ms, 30);
        let kp = load_keypair(active_address, wallet_path)?;
        let user_sig = sign_personal_message(&kp, msg.as_bytes())?;

        let certificate = Certificate {
            user: parse_address(active_address)?,
            session_vk: session_pk.clone(),
            creation_time: creation_time_ms,
            ttl_min: 30,
            signature: user_sig,
            mvr_name: None,
        };

        let identity = seal_id(package_id, repo_id);
        let ptb = build_seal_approve_ptb(package_id, &identity, acl_id, acl_initial_version)?;

        let req_bytes = signed_request(&ptb, &enc_pk, &enc_vk);
        let request_sig: Ed25519Signature = session_kp.sign(&req_bytes);

        let ptb_b64 = base64::engine::general_purpose::STANDARD
            .encode(bcs::to_bytes(&ptb).map_err(WalGitError::from)?);

        let fetch_req = FetchKeyRequest {
            ptb: ptb_b64,
            enc_key: enc_pk,
            enc_verification_key: enc_vk,
            request_signature: request_sig,
            certificate,
        };

        let response = post_fetch_key(&self.key_server_url, &fetch_req).await?;
        let seal_responses = vec![(server_obj_id, response)];

        let cached_keys = decrypt_seal_responses(&enc_sk, &seal_responses, &server_pk_map)
            .map_err(|e| WalGitError::SealDecrypt(format!("combine shares: {:?}", e)))?;

        seal_sdk::seal_decrypt_object(&encrypted_obj, &cached_keys, &server_pk_map)
            .map_err(|e| WalGitError::SealDecrypt(format!("decrypt: {:?}", e)))
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn parse_address(hex: &str) -> Result<Address> {
    Address::from_hex(hex)
        .map_err(|e| WalGitError::SealKeyServer(format!("bad address {}: {}", hex, e)))
}

async fn fetch_ibe_public_key(graphql_url: &str, server_id: &str) -> Result<IBEPublicKey> {
    let client = sui_graphql::Client::new(graphql_url)
        .map_err(|e| WalGitError::SealKeyServer(format!("graphql client: {}", e)))?;
    // Key server config is stored as dynamic field keyed by u64=1.
    let key_bcs = base64::engine::general_purpose::STANDARD.encode(1u64.to_le_bytes());

    let response = client
        .query::<serde_json::Value>(
            r#"query($parent: SuiAddress!, $bcs: Base64!) {
              address(address: $parent) {
                dynamicField(name: { type: "u64", bcs: $bcs }) {
                  value { ... on MoveValue { json } }
                }
              }
            }"#,
            json!({ "parent": server_id, "bcs": key_bcs }),
        )
        .await
        .map_err(|e| WalGitError::SealKeyServer(format!("graphql request: {}", e)))?;

    if response.has_errors() {
        return Err(WalGitError::SealKeyServer(
            response
                .errors()
                .first()
                .map(|e| e.message().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        ));
    }

    let data = response
        .data()
        .cloned()
        .ok_or_else(|| WalGitError::SealKeyServer("no data".to_string()))?;

    let pk_b64 = data["address"]["dynamicField"]["value"]["json"]["pk"]
        .as_str()
        .ok_or_else(|| WalGitError::SealKeyServer("pk field missing".to_string()))?;

    let pk_bytes = base64::engine::general_purpose::STANDARD
        .decode(pk_b64)
        .map_err(|e| WalGitError::SealKeyServer(format!("pk base64 decode: {}", e)))?;

    let arr: [u8; G2_ELEMENT_BYTE_LENGTH] = pk_bytes
        .try_into()
        .map_err(|_| WalGitError::SealKeyServer(format!(
            "pk must be {} bytes",
            G2_ELEMENT_BYTE_LENGTH
        )))?;
    G2Element::from_byte_array(&arr)
        .map_err(|e| WalGitError::SealKeyServer(format!("pk parse: {:?}", e)))
}

fn build_seal_approve_ptb(
    package_id: &str,
    identity: &[u8],
    acl_id: &str,
    acl_initial_version: u64,
) -> Result<ProgrammableTransaction> {
    let id_bcs = bcs::to_bytes(&identity.to_vec()).map_err(WalGitError::from)?;
    let acl_address = parse_address(acl_id)?;
    let pkg_address = parse_address(package_id)?;

    let inputs = vec![
        Input::Pure(id_bcs),
        Input::Shared(SharedInput::new(acl_address, acl_initial_version, false)),
    ];
    let commands = vec![Command::MoveCall(MoveCall {
        package: pkg_address,
        module: Identifier::new("walgit")
            .map_err(|e| WalGitError::SealKeyServer(format!("module ident: {}", e)))?,
        function: Identifier::new("seal_approve")
            .map_err(|e| WalGitError::SealKeyServer(format!("func ident: {}", e)))?,
        type_arguments: vec![],
        arguments: vec![Argument::Input(0), Argument::Input(1)],
    })];

    Ok(ProgrammableTransaction { inputs, commands })
}

async fn post_fetch_key(server_url: &str, req: &FetchKeyRequest) -> Result<FetchKeyResponse> {
    let body = req
        .to_json_string()
        .map_err(|e| WalGitError::SealKeyServer(format!("serialize req: {}", e)))?;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/fetch_key", server_url))
        .header("Client-Sdk-Type", "rust")
        .header("Client-Sdk-Version", "0.0.0")
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| WalGitError::SealKeyServer(format!("connect: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(WalGitError::SealKeyServer(format!(
            "HTTP {}: {}",
            status, text
        )));
    }

    resp.json::<FetchKeyResponse>()
        .await
        .map_err(|e| WalGitError::SealKeyServer(format!("parse response: {}", e)))
}

/// Sign `message` as a Sui PersonalMessage using the user's Ed25519 wallet key.
fn sign_personal_message(kp: &KeyPair, message: &[u8]) -> Result<UserSignature> {
    let msg_bcs = bcs::to_bytes(&message.to_vec()).map_err(WalGitError::from)?;
    // Intent prefix for PersonalMessage / V0 / Sui
    let mut intent_msg = vec![3u8, 0u8, 0u8];
    intent_msg.extend_from_slice(&msg_bcs);

    let mut hasher = Blake2b::<U32>::new();
    blake2::Digest::update(&mut hasher, &intent_msg);
    let digest = hasher.finalize();

    let signing_key = ed25519_dalek::SigningKey::from_bytes(&kp.private_key);
    let sig = signing_key.sign(&digest);

    Ok(UserSignature::Simple(SimpleSignature::Ed25519 {
        signature: SuiEd25519Signature::new(sig.to_bytes()),
        public_key: SuiEd25519PublicKey::new(kp.public_key),
    }))
}
