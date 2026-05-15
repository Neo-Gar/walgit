// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Native PTB construction, signing, and execution via Sui GraphQL.
//!
//! We deliberately do not depend on the Sui CLI: keystore reading is in
//! [`super::keystore`], gas-coin lookup goes through GraphQL, and execution
//! uses `sui_graphql::Client::execute_transaction`.

use crate::error::{Result, WalGitError};
use crate::sui::keystore::KeyPair;
use crate::sui::types::GasCost;
use blake2::{Blake2b, digest::consts::U32};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sui_graphql::Client as GqlClient;
use sui_sdk_types::{
    Address, Digest as SuiDigest, Ed25519PublicKey, Ed25519Signature, GasPayment, Identifier,
    Input, MoveCall, ObjectReference, ProgrammableTransaction, SimpleSignature, Transaction,
    TransactionExpiration, TransactionKind, UserSignature,
};

/// Sui Clock object — always shared, initial version 1.
pub const CLOCK_OBJECT_ID: &str = "0x6";
pub const CLOCK_INITIAL_SHARED_VERSION: u64 = 1;

/// Conservative default budget: 50 MIST × 1000 = 0.00005 SUI. Tune per-call if needed.
pub const DEFAULT_GAS_BUDGET: u64 = 50_000_000;

/// Argument descriptor used by the high-level builders. The PTB builder turns
/// this into the matching `Input` and `Argument` references.
pub enum Arg {
    /// BCS-encoded pure value (string, u64, bool, vector<u8>, …).
    Pure(Vec<u8>),
    /// Shared object — `(object_id_hex, initial_shared_version, mutable)`.
    Shared {
        id: String,
        initial_shared_version: u64,
        mutable: bool,
    },
}

impl Arg {
    pub fn pure<T: serde::Serialize>(v: &T) -> Result<Self> {
        Ok(Arg::Pure(bcs::to_bytes(v)?))
    }

    pub fn shared(id: impl Into<String>, initial_shared_version: u64, mutable: bool) -> Self {
        Arg::Shared {
            id: id.into(),
            initial_shared_version,
            mutable,
        }
    }

    pub fn clock() -> Self {
        Arg::Shared {
            id: CLOCK_OBJECT_ID.to_string(),
            initial_shared_version: CLOCK_INITIAL_SHARED_VERSION,
            mutable: false,
        }
    }
}

/// Result of a successful transaction execution.
pub struct ExecResult {
    pub gas: GasCost,
    pub created_objects: Vec<CreatedObject>,
    pub digest: Option<String>,
}

pub struct CreatedObject {
    pub object_id: String,
    pub object_type: String,
}

impl ExecResult {
    pub fn find_created(&self, type_suffix: &str) -> Option<&CreatedObject> {
        self.created_objects
            .iter()
            .find(|o| o.object_type.contains(type_suffix))
    }
}

/// Build, sign, and execute a single Move call as a programmable transaction.
pub async fn execute_move_call(
    graphql: &GqlClient,
    graphql_url: &str,
    keypair: &KeyPair,
    package_id: &str,
    module: &str,
    function: &str,
    args: Vec<Arg>,
    gas_budget: u64,
) -> Result<ExecResult> {
    let sender = parse_address(&keypair.address)?;
    let package = parse_address(package_id)?;

    let mut inputs = Vec::with_capacity(args.len());
    let mut arg_refs = Vec::with_capacity(args.len());
    for (i, a) in args.into_iter().enumerate() {
        let input = match a {
            Arg::Pure(bytes) => Input::Pure(bytes),
            Arg::Shared {
                id,
                initial_shared_version,
                mutable,
            } => Input::Shared(sui_sdk_types::SharedInput::new(
                parse_address(&id)?,
                initial_shared_version,
                mutable,
            )),
        };
        inputs.push(input);
        arg_refs.push(sui_sdk_types::Argument::Input(i as u16));
    }

    let commands = vec![sui_sdk_types::Command::MoveCall(MoveCall {
        package,
        module: ident(module)?,
        function: ident(function)?,
        type_arguments: vec![],
        arguments: arg_refs,
    })];

    let ptb = ProgrammableTransaction { inputs, commands };

    let (gas_objects, gas_price) =
        tokio::try_join!(get_gas_coins(graphql_url, &keypair.address, gas_budget), get_reference_gas_price(graphql_url))?;

    let tx = Transaction {
        kind: TransactionKind::ProgrammableTransaction(ptb),
        sender,
        gas_payment: GasPayment {
            objects: gas_objects,
            owner: sender,
            price: gas_price,
            budget: gas_budget,
        },
        expiration: TransactionExpiration::None,
    };

    let signature = sign_transaction(&tx, keypair)?;

    let result = graphql
        .execute_transaction(&tx, &[signature])
        .await
        .map_err(|e| WalGitError::sui_transaction(format!("execute failed: {}", e)))?;

    let effects = result
        .effects
        .ok_or_else(|| WalGitError::sui_transaction("no effects returned".to_string()))?;

    parse_exec_result(effects).await
}

/// BCS-encode the transaction with the TransactionData intent prefix [0,0,0],
/// hash with Blake2b-256, sign with Ed25519, and wrap as a Sui UserSignature.
pub fn sign_transaction(tx: &Transaction, keypair: &KeyPair) -> Result<UserSignature> {
    let tx_bytes = bcs::to_bytes(tx)?;
    let mut msg = Vec::with_capacity(3 + tx_bytes.len());
    msg.extend_from_slice(&[0u8, 0u8, 0u8]); // intent: TransactionData / V0 / Sui
    msg.extend_from_slice(&tx_bytes);

    use blake2::Digest;
    let mut hasher = Blake2b::<U32>::new();
    hasher.update(&msg);
    let digest = hasher.finalize();

    let signing_key = SigningKey::from_bytes(&keypair.private_key);
    let sig = signing_key.sign(&digest);

    Ok(UserSignature::Simple(SimpleSignature::Ed25519 {
        signature: Ed25519Signature::new(sig.to_bytes()),
        public_key: Ed25519PublicKey::new(keypair.public_key),
    }))
}

fn ident(s: &str) -> Result<Identifier> {
    Identifier::new(s).map_err(|e| WalGitError::sui_transaction(format!("bad identifier '{}': {}", s, e)))
}

fn parse_address(hex: &str) -> Result<Address> {
    Address::from_hex(hex).map_err(|e| {
        WalGitError::sui_transaction(format!("invalid Sui address '{}': {}", hex, e))
    })
}

fn parse_digest(b58: &str) -> Result<SuiDigest> {
    SuiDigest::from_base58(b58)
        .map_err(|e| WalGitError::sui_transaction(format!("invalid digest '{}': {:?}", b58, e)))
}

// ─── GraphQL helpers for tx assembly ─────────────────────────────────────────

async fn graphql_query(url: &str, query: &str, variables: Value) -> Result<Value> {
    let client = reqwest::Client::new();
    let body = json!({ "query": query, "variables": variables });
    let resp = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&body)?)
        .send()
        .await
        .map_err(|e| WalGitError::sui_network(format!("graphql request failed: {}", e)))?;

    if !resp.status().is_success() {
        return Err(WalGitError::sui_network(format!(
            "graphql HTTP {}",
            resp.status()
        )));
    }
    let v: Value = resp.json().await?;
    if let Some(errs) = v["errors"].as_array() {
        if !errs.is_empty() {
            let msg = errs
                .iter()
                .filter_map(|e| e["message"].as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(WalGitError::sui_graphql(msg));
        }
    }
    Ok(v["data"].clone())
}

pub async fn get_reference_gas_price(graphql_url: &str) -> Result<u64> {
    let data = graphql_query(
        graphql_url,
        r#"query { epoch { referenceGasPrice } }"#,
        json!({}),
    )
    .await?;
    let rgp_str = data["epoch"]["referenceGasPrice"]
        .as_str()
        .or_else(|| data["epoch"]["referenceGasPrice"]["value"].as_str());
    if let Some(s) = rgp_str {
        return s
            .parse::<u64>()
            .map_err(|e| WalGitError::sui_graphql(format!("rgp parse: {}", e)));
    }
    if let Some(n) = data["epoch"]["referenceGasPrice"].as_u64() {
        return Ok(n);
    }
    Err(WalGitError::sui_graphql(
        "referenceGasPrice not in epoch response".to_string(),
    ))
}

/// Find one or more SUI coins owned by `address` with combined balance >= `min_total`.
/// Returns `ObjectReference`s suitable for `GasPayment.objects`.
pub async fn get_gas_coins(
    graphql_url: &str,
    address: &str,
    min_total: u64,
) -> Result<Vec<ObjectReference>> {
    let data = graphql_query(
        graphql_url,
        r#"query($owner: SuiAddress!) {
          address(address: $owner) {
            coins(type: "0x2::sui::SUI", first: 25) {
              nodes {
                address
                version
                digest
                contents { json }
              }
            }
          }
        }"#,
        json!({ "owner": address }),
    )
    .await?;

    let empty: Vec<Value> = vec![];
    let nodes = data["address"]["coins"]["nodes"]
        .as_array()
        .unwrap_or(&empty);

    let mut picked: Vec<ObjectReference> = vec![];
    let mut total: u128 = 0;
    for node in nodes {
        let id = node["address"].as_str().unwrap_or("");
        let version = node["version"]
            .as_u64()
            .or_else(|| node["version"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);
        let digest = node["digest"].as_str().unwrap_or("");
        let balance: u128 = node["contents"]["json"]["balance"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| node["contents"]["json"]["balance"].as_u64().map(|n| n as u128))
            .unwrap_or(0);
        if id.is_empty() || digest.is_empty() {
            continue;
        }
        picked.push(ObjectReference::new(
            parse_address(id)?,
            version,
            parse_digest(digest)?,
        ));
        total += balance;
        if total >= min_total as u128 {
            break;
        }
    }

    if picked.is_empty() {
        return Err(WalGitError::sui_network(format!(
            "no SUI coins for address {}",
            address
        )));
    }
    if total < min_total as u128 {
        return Err(WalGitError::InsufficientGas);
    }
    Ok(picked)
}

async fn parse_exec_result(effects: sui_sdk_types::TransactionEffects) -> Result<ExecResult> {
    use sui_sdk_types::{IdOperation, TransactionEffects};

    let (gas, created_ids, digest) = match effects {
        TransactionEffects::V1(e) => {
            let gas = GasCost {
                computation_mist: e.gas_used.computation_cost,
                storage_mist: e.gas_used.storage_cost,
                rebate_mist: e.gas_used.storage_rebate,
            };
            // v1 has been retired on testnet/mainnet; we don't bother parsing it.
            (gas, vec![], Some(format!("{}", e.transaction_digest)))
        }
        TransactionEffects::V2(e) => {
            let gas = GasCost {
                computation_mist: e.gas_used.computation_cost,
                storage_mist: e.gas_used.storage_cost,
                rebate_mist: e.gas_used.storage_rebate,
            };
            let mut created = Vec::new();
            for ch in &e.changed_objects {
                if matches!(ch.id_operation, IdOperation::Created) {
                    created.push(format!("{}", ch.object_id));
                }
            }
            (gas, created, Some(format!("{}", e.transaction_digest)))
        }
    };

    Ok(ExecResult {
        gas,
        created_objects: created_ids
            .into_iter()
            .map(|object_id| CreatedObject {
                object_id,
                object_type: String::new(),
            })
            .collect(),
        digest,
    })
}

/// Look up the Move type string for each object id. Used to disambiguate
/// "which created object is the Repository" after a multi-object PTB.
pub async fn fetch_object_types(
    graphql_url: &str,
    ids: &[String],
) -> Result<std::collections::HashMap<String, String>> {
    use std::collections::HashMap;
    let mut out = HashMap::new();
    for id in ids {
        let data = graphql_query(
            graphql_url,
            r#"query($id: SuiAddress!) {
              object(address: $id) {
                asMoveObject { contents { type { repr } } }
              }
            }"#,
            json!({ "id": id }),
        )
        .await?;
        if let Some(t) = data["object"]["asMoveObject"]["contents"]["type"]["repr"].as_str() {
            out.insert(id.clone(), t.to_string());
        }
    }
    Ok(out)
}

/// Convenience: execute a Move call and populate object types on the result.
pub async fn execute_and_resolve_types(
    graphql: &GqlClient,
    graphql_url: &str,
    keypair: &KeyPair,
    package_id: &str,
    module: &str,
    function: &str,
    args: Vec<Arg>,
    gas_budget: u64,
) -> Result<ExecResult> {
    let mut result = execute_move_call(
        graphql,
        graphql_url,
        keypair,
        package_id,
        module,
        function,
        args,
        gas_budget,
    )
    .await?;
    let ids: Vec<String> = result
        .created_objects
        .iter()
        .map(|o| o.object_id.clone())
        .collect();
    if !ids.is_empty() {
        let types = fetch_object_types(graphql_url, &ids).await?;
        for obj in result.created_objects.iter_mut() {
            if let Some(t) = types.get(&obj.object_id) {
                obj.object_type = t.clone();
            }
        }
    }
    Ok(result)
}
