// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! GraphQL read queries against a Sui fullnode.

use crate::error::{Result, WalGitError};
use crate::sui::types::{AccessRecord, CommitRecord, PullRequestRecord, RepoRecord};
use base64::Engine as _;
use serde_json::{Value, json};
use std::collections::HashMap;
use sui_graphql::Client as GqlClient;

pub struct Queries {
    client: GqlClient,
}

impl Queries {
    pub fn new(graphql_url: &str) -> Result<Self> {
        let client = GqlClient::new(graphql_url).map_err(|e| {
            WalGitError::sui_graphql(format!("invalid GraphQL URL '{}': {}", graphql_url, e))
        })?;
        Ok(Self { client })
    }

    async fn raw(&self, query: &str, variables: Value) -> Result<Value> {
        let response = self
            .client
            .query::<Value>(query, variables)
            .await
            .map_err(|e| WalGitError::sui_graphql(format!("request failed: {}", e)))?;

        if response.has_errors() {
            let msg = response
                .errors()
                .first()
                .map(|e| e.message())
                .unwrap_or("unknown");
            return Err(WalGitError::sui_graphql(msg.to_string()));
        }

        response
            .data()
            .cloned()
            .ok_or_else(|| WalGitError::sui_graphql("empty data".to_string()))
    }

    /// Fetch a Move object and return its fields as JSON.
    pub async fn get_object(&self, id: &str) -> Result<Value> {
        let data = self
            .raw(
                r#"query($id: SuiAddress!) {
                  object(address: $id) {
                    asMoveObject { contents { json } }
                  }
                }"#,
                json!({ "id": id }),
            )
            .await?;
        data["object"]["asMoveObject"]["contents"]["json"]
            .as_object()
            .cloned()
            .map(Value::Object)
            .ok_or_else(|| WalGitError::ObjectNotFound(id.to_string()))
    }

    /// Get the `initialSharedVersion` of a shared Sui object — needed for PTB inputs.
    pub async fn get_initial_shared_version(&self, id: &str) -> Result<u64> {
        let data = self
            .raw(
                r#"query($id: SuiAddress!) {
                  object(address: $id) {
                    owner { ... on Shared { initialSharedVersion } }
                  }
                }"#,
                json!({ "id": id }),
            )
            .await?;
        data["object"]["owner"]["initialSharedVersion"]
            .as_u64()
            .ok_or_else(|| {
                WalGitError::sui_graphql(format!(
                    "initialSharedVersion missing for {} (not a shared object?)",
                    id
                ))
            })
    }

    pub async fn get_access_control(&self, acl_id: &str) -> Result<AccessRecord> {
        let fields = self.get_object(acl_id).await?;
        let parse_addrs = |key: &str| -> Vec<String> {
            fields[key]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        Ok(AccessRecord {
            acl_id: acl_id.to_string(),
            owner: fields["owner"].as_str().unwrap_or("").to_string(),
            allowed_readers: parse_addrs("allowed_readers"),
            allowed_writers: parse_addrs("allowed_writers"),
        })
    }

    async fn dynamic_string_field(
        &self,
        parent_id: &str,
        key: &str,
    ) -> Result<Option<String>> {
        let bcs = bcs_string_b64(key);
        let data = self
            .raw(
                r#"query($parent: SuiAddress!, $bcs: Base64!) {
                  address(address: $parent) {
                    dynamicField(name: { type: "0x1::string::String", bcs: $bcs }) {
                      value {
                        ... on MoveValue { json }
                        ... on MoveObject { address }
                      }
                    }
                  }
                }"#,
                json!({ "parent": parent_id, "bcs": bcs }),
            )
            .await?;

        let field = &data["address"]["dynamicField"];
        if field.is_null() {
            return Ok(None);
        }
        let v = &field["value"];
        if let Some(s) = v["json"].as_str() {
            return Ok(Some(s.to_string()));
        }
        if let Some(s) = v["address"].as_str() {
            return Ok(Some(s.to_string()));
        }
        Ok(None)
    }

    /// Walk the branches `Table<String, ID>` and return every (name, commit_id) pair.
    pub async fn get_branches_table(&self, table_id: &str) -> HashMap<String, String> {
        let mut branches = HashMap::new();
        let mut cursor: Option<String> = None;

        loop {
            let Ok(data) = self
                .raw(
                    r#"query($table: SuiAddress!, $cursor: String) {
                      address(address: $table) {
                        dynamicFields(first: 50, after: $cursor) {
                          nodes {
                            name { json }
                            value { ... on MoveValue { json } ... on MoveObject { address } }
                          }
                          pageInfo { hasNextPage endCursor }
                        }
                      }
                    }"#,
                    json!({ "table": table_id, "cursor": cursor }),
                )
                .await
            else {
                break;
            };

            let df = &data["address"]["dynamicFields"];
            let empty = vec![];
            for node in df["nodes"].as_array().unwrap_or(&empty) {
                let Some(name) = node["name"]["json"].as_str() else {
                    continue;
                };
                let commit_id = node["value"]["json"]
                    .as_str()
                    .or_else(|| node["value"]["address"].as_str())
                    .unwrap_or("");
                if !commit_id.is_empty() {
                    branches.insert(name.to_string(), commit_id.to_string());
                }
            }

            if df["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false) {
                cursor = df["pageInfo"]["endCursor"].as_str().map(String::from);
            } else {
                break;
            }
        }

        branches
    }

    pub async fn get_repo_branch_head(
        &self,
        repo_id: &str,
        branch: &str,
    ) -> Result<Option<String>> {
        let fields = self.get_object(repo_id).await?;
        let table_id = fields["branches"]["id"]["id"]
            .as_str()
            .or_else(|| fields["branches"]["id"].as_str())
            .ok_or_else(|| {
                WalGitError::sui_graphql(format!(
                    "could not read branches table ID from repository {}",
                    repo_id
                ))
            })?;
        self.dynamic_string_field(table_id, branch).await
    }

    pub async fn get_repo_by_id(&self, repo_id: &str, fallback_owner: &str) -> Result<RepoRecord> {
        let fields = self.get_object(repo_id).await.map_err(|_| {
            WalGitError::RepoNotFound(format!("object {}", repo_id))
        })?;

        let name = fields["name"].as_str().unwrap_or("").to_string();
        let acl_id = fields["acl_id"]
            .as_str()
            .or_else(|| fields["acl_id"]["id"].as_str())
            .unwrap_or("")
            .to_string();
        let table_id = fields["branches"]["id"]["id"]
            .as_str()
            .or_else(|| fields["branches"]["id"].as_str())
            .unwrap_or("");

        let branches = self.get_branches_table(table_id).await;

        Ok(RepoRecord {
            id: repo_id.to_string(),
            acl_id,
            owner: fields["owner"].as_str().unwrap_or(fallback_owner).to_string(),
            name,
            description: fields["description"].as_str().unwrap_or("").to_string(),
            is_private: fields["is_private"].as_bool().unwrap_or(false),
            branches,
        })
    }

    pub async fn get_repo_by_owner_name(
        &self,
        package_id: &str,
        owner: &str,
        name: &str,
    ) -> Result<Option<RepoRecord>> {
        let repo_type = format!("{}::walgit::Repository", package_id);
        let mut cursor: Option<String> = None;

        loop {
            let data = self
                .raw(
                    r#"query($owner: SuiAddress!, $type: String!, $cursor: String) {
                      objects(
                        filter: { owner: $owner, type: $type }
                        first: 50
                        after: $cursor
                      ) {
                        nodes { address asMoveObject { contents { json } } }
                        pageInfo { hasNextPage endCursor }
                      }
                    }"#,
                    json!({ "owner": owner, "type": repo_type, "cursor": cursor }),
                )
                .await?;

            let objects = &data["objects"];
            let Some(nodes) = objects["nodes"].as_array() else {
                break;
            };

            for node in nodes {
                let fields = &node["asMoveObject"]["contents"]["json"];
                if fields["name"].as_str() != Some(name) {
                    continue;
                }
                if fields["branches"].is_null() {
                    continue;
                }
                let repo_id = node["address"].as_str().unwrap_or("").to_string();
                return Ok(Some(self.get_repo_by_id(&repo_id, owner).await?));
            }

            if objects["pageInfo"]["hasNextPage"]
                .as_bool()
                .unwrap_or(false)
            {
                cursor = objects["pageInfo"]["endCursor"]
                    .as_str()
                    .map(String::from);
            } else {
                break;
            }
        }

        // Fallback: search transaction history for create_repository/fork_repository calls.
        // Required for shared Repository objects that don't appear in address.objects.
        for func_name in &["create_repository", "fork_repository"] {
            let func_filter = format!("{}::walgit::{}", package_id, func_name);
            let data = self
                .raw(
                    r#"query($owner: SuiAddress!, $func: String!) {
                      transactions(
                        filter: { sentAddress: $owner, function: $func }
                        last: 50
                      ) {
                        nodes {
                          effects {
                            objectChanges {
                              nodes {
                                address
                                outputState { asMoveObject { contents { json } } }
                              }
                            }
                          }
                        }
                      }
                    }"#,
                    json!({ "owner": owner, "func": func_filter }),
                )
                .await?;

            let empty = vec![];
            let txns = data["transactions"]["nodes"].as_array().unwrap_or(&empty);
            for txn in txns.iter().rev() {
                let changes = txn["effects"]["objectChanges"]["nodes"]
                    .as_array()
                    .unwrap_or(&empty);
                for change in changes {
                    let fields = &change["outputState"]["asMoveObject"]["contents"]["json"];
                    if fields["name"].as_str() != Some(name) || fields["branches"].is_null() {
                        continue;
                    }
                    let repo_id = change["address"].as_str().unwrap_or("").to_string();
                    return Ok(Some(self.get_repo_by_id(&repo_id, owner).await?));
                }
            }
        }

        Ok(None)
    }

    pub async fn get_commit_chain(
        &self,
        head_commit_id: &str,
        limit: usize,
    ) -> Result<Vec<CommitRecord>> {
        let mut commits = Vec::new();
        let mut current = Some(head_commit_id.to_string());

        while let Some(id) = current {
            if commits.len() >= limit {
                break;
            }
            let Ok(fields) = self.get_object(&id).await else {
                break;
            };
            let parent = parse_option_id(&fields["parent"]);
            commits.push(CommitRecord {
                id: id.clone(),
                blob_id: fields["blob_id"].as_str().unwrap_or("").to_string(),
                git_head: fields["git_head"].as_str().unwrap_or("").to_string(),
                parent: parent.clone(),
                message: fields["message"].as_str().unwrap_or("").to_string(),
                author: fields["author"].as_str().unwrap_or("").to_string(),
                timestamp: fields["timestamp"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .or_else(|| fields["timestamp"].as_u64())
                    .unwrap_or(0),
            });
            current = parent;
        }
        Ok(commits)
    }

    /// Check whether `forked_by` has already forked `original_repo_id`.
    pub async fn find_fork_of(
        &self,
        package_id: &str,
        original_repo_id: &str,
        forked_by: &str,
    ) -> Result<Option<(String, String)>> {
        let event_type = format!("{}::walgit::RepositoryForked", package_id);
        let mut cursor: Option<String> = None;

        loop {
            let data = self
                .raw(
                    r#"query($type: String!, $sender: SuiAddress!, $cursor: String) {
                      events(filter: { type: $type, sender: $sender }, first: 50, after: $cursor) {
                        nodes { contents { json } }
                        pageInfo { hasNextPage endCursor }
                      }
                    }"#,
                    json!({ "type": event_type, "sender": forked_by, "cursor": cursor }),
                )
                .await?;

            let empty = vec![];
            let nodes = data["events"]["nodes"].as_array().unwrap_or(&empty);

            for node in nodes {
                let f = &node["contents"]["json"];
                let orig = f["original_repo_id"]
                    .as_str()
                    .or_else(|| f["original_repo_id"]["bytes"].as_str())
                    .unwrap_or("");
                if orig != original_repo_id {
                    continue;
                }
                let fork_id = f["fork_repo_id"]
                    .as_str()
                    .or_else(|| f["fork_repo_id"]["bytes"].as_str())
                    .unwrap_or("")
                    .to_string();
                let fork_name = f["fork_name"].as_str().unwrap_or("").to_string();
                return Ok(Some((fork_id, fork_name)));
            }

            if data["events"]["pageInfo"]["hasNextPage"]
                .as_bool()
                .unwrap_or(false)
            {
                cursor = data["events"]["pageInfo"]["endCursor"]
                    .as_str()
                    .map(String::from);
            } else {
                break;
            }
        }

        Ok(None)
    }

    pub async fn list_pull_requests(
        &self,
        package_id: &str,
        repo_id: &str,
    ) -> Result<Vec<PullRequestRecord>> {
        let event_type = format!("{}::pull_request::PRCreated", package_id);
        let mut cursor: Option<String> = None;
        let mut summaries: Vec<(String, u64, String, String, String, u64)> = Vec::new();

        loop {
            let data = self
                .raw(
                    r#"query($type: String!, $cursor: String) {
                      events(filter: { type: $type }, first: 50, after: $cursor) {
                        nodes { contents { json } }
                        pageInfo { hasNextPage endCursor }
                      }
                    }"#,
                    json!({ "type": event_type, "cursor": cursor }),
                )
                .await?;

            let empty = vec![];
            let nodes = data["events"]["nodes"].as_array().unwrap_or(&empty);

            for node in nodes {
                let f = &node["contents"]["json"];
                let event_repo_id = f["repo_id"]
                    .as_str()
                    .or_else(|| f["repo_id"]["bytes"].as_str())
                    .unwrap_or("");
                if event_repo_id != repo_id {
                    continue;
                }
                let pr_id = f["pr_id"]
                    .as_str()
                    .or_else(|| f["pr_id"]["bytes"].as_str())
                    .unwrap_or("")
                    .to_string();
                let number: u64 = f["number"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .or_else(|| f["number"].as_u64())
                    .unwrap_or(0);
                let created_at: u64 = f["created_at"]
                    .as_str()
                    .and_then(|s| s.parse().ok())
                    .or_else(|| f["created_at"].as_u64())
                    .unwrap_or(0);
                summaries.push((
                    pr_id,
                    number,
                    f["author"].as_str().unwrap_or("").to_string(),
                    f["source_branch"].as_str().unwrap_or("").to_string(),
                    f["target_branch"].as_str().unwrap_or("").to_string(),
                    created_at,
                ));
            }

            if data["events"]["pageInfo"]["hasNextPage"]
                .as_bool()
                .unwrap_or(false)
            {
                cursor = data["events"]["pageInfo"]["endCursor"]
                    .as_str()
                    .map(String::from);
            } else {
                break;
            }
        }

        let mut records = Vec::with_capacity(summaries.len());
        for (id, number, author, source_branch, target_branch, created_at) in summaries {
            if id.is_empty() {
                continue;
            }
            let mut rec = PullRequestRecord {
                id: id.clone(),
                repo_id: repo_id.to_string(),
                number,
                author,
                source_branch,
                target_branch,
                source_blob_id: String::new(),
                status: 0,
                approved: false,
                approved_by: None,
                merge_commit_blob_id: None,
                merged_by: None,
                created_at,
            };
            if let Ok(full) = self.get_pull_request(&id).await {
                rec.status = full.status;
                rec.approved = full.approved;
                rec.source_blob_id = full.source_blob_id;
            }
            records.push(rec);
        }
        records.sort_by_key(|r| r.number);
        Ok(records)
    }

    pub async fn get_pull_request(&self, pr_id: &str) -> Result<PullRequestRecord> {
        let fields = self.get_object(pr_id).await?;

        let repo_id = fields["repo_id"]
            .as_str()
            .or_else(|| fields["repo_id"]["id"].as_str())
            .unwrap_or("")
            .to_string();
        let number: u64 = fields["number"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| fields["number"].as_u64())
            .unwrap_or(0);
        let status: u8 = fields["status"]
            .as_u64()
            .map(|n| n as u8)
            .or_else(|| fields["status"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);
        let created_at: u64 = fields["created_at"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| fields["created_at"].as_u64())
            .unwrap_or(0);

        Ok(PullRequestRecord {
            id: pr_id.to_string(),
            repo_id,
            number,
            author: fields["author"].as_str().unwrap_or("").to_string(),
            source_branch: fields["source_branch"].as_str().unwrap_or("").to_string(),
            target_branch: fields["target_branch"].as_str().unwrap_or("").to_string(),
            source_blob_id: fields["source_blob_id"].as_str().unwrap_or("").to_string(),
            status,
            approved: fields["approved"].as_bool().unwrap_or(false),
            approved_by: parse_option_str(&fields["approved_by"]),
            merge_commit_blob_id: parse_option_str(&fields["merge_commit_blob_id"]),
            merged_by: parse_option_str(&fields["merged_by"]),
            created_at,
        })
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// BCS-encode a `String` value (length prefix as ULEB128 + bytes) and base64-encode.
fn bcs_string_b64(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut encoded = Vec::with_capacity(1 + bytes.len());
    let mut val = bytes.len();
    loop {
        let mut byte = (val & 0x7f) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80;
        }
        encoded.push(byte);
        if val == 0 {
            break;
        }
    }
    encoded.extend_from_slice(bytes);
    base64::engine::general_purpose::STANDARD.encode(&encoded)
}

fn parse_option_str(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    if let Some(s) = value.as_str() {
        return if s.is_empty() { None } else { Some(s.to_string()) };
    }
    if let Some(vec) = value["vec"].as_array() {
        return vec.first().and_then(|v| v.as_str()).map(String::from);
    }
    if let Some(s) = value["some"].as_str() {
        return Some(s.to_string());
    }
    None
}

fn parse_option_id(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    if let Some(s) = value.as_str() {
        return if s.is_empty() { None } else { Some(s.to_string()) };
    }
    if let Some(vec) = value["vec"].as_array() {
        return vec.first().and_then(|v| v.as_str()).map(String::from);
    }
    None
}
