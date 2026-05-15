// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! High-level Sui client tying queries and writes together.
//! Exposes one method per WalGit on-chain operation.

use crate::error::{Result, WalGitError};
use crate::retry::{RetryPolicy, with_backoff};
use crate::sui::keystore::KeyPair;
use crate::sui::queries::Queries;
use crate::sui::tx::{self, Arg, ExecResult};
use crate::sui::types::{
    AccessRecord, CommitRecord, GasCost, PullRequestRecord, RepoRecord,
};
use std::sync::Arc;
use sui_graphql::Client as GqlClient;

pub struct SuiClient {
    graphql_url: Arc<str>,
    pub queries: Queries,
    gql_client: Arc<GqlClient>,
    retry: RetryPolicy,
}

impl SuiClient {
    pub fn new(graphql_url: String) -> Result<Self> {
        Self::with_retry(graphql_url, RetryPolicy::default())
    }

    pub fn with_retry(graphql_url: String, retry: RetryPolicy) -> Result<Self> {
        let gql_client = GqlClient::new(&graphql_url)
            .map_err(|e| WalGitError::sui_graphql(format!("invalid GraphQL URL: {}", e)))?;
        let queries = Queries::new(&graphql_url)?;
        Ok(Self {
            graphql_url: graphql_url.into(),
            queries,
            gql_client: Arc::new(gql_client),
            retry,
        })
    }

    // ─── Read passthroughs (with retry for transient GraphQL errors) ────────

    pub async fn get_object(&self, id: &str) -> Result<serde_json::Value> {
        let id = id.to_string();
        with_backoff(self.retry, move || {
            let q = &self.queries;
            let id = id.clone();
            async move { q.get_object(&id).await }
        })
        .await
    }

    pub async fn get_repo_by_id(&self, repo_id: &str, owner_fallback: &str) -> Result<RepoRecord> {
        self.queries.get_repo_by_id(repo_id, owner_fallback).await
    }

    pub async fn get_repo_by_owner_name(
        &self,
        package_id: &str,
        owner: &str,
        name: &str,
    ) -> Result<Option<RepoRecord>> {
        self.queries
            .get_repo_by_owner_name(package_id, owner, name)
            .await
    }

    pub async fn get_access_control(&self, acl_id: &str) -> Result<AccessRecord> {
        self.queries.get_access_control(acl_id).await
    }

    pub async fn get_repo_branch_head(
        &self,
        repo_id: &str,
        branch: &str,
    ) -> Result<Option<String>> {
        self.queries.get_repo_branch_head(repo_id, branch).await
    }

    pub async fn get_commit_chain(
        &self,
        head_commit_id: &str,
        limit: usize,
    ) -> Result<Vec<CommitRecord>> {
        self.queries.get_commit_chain(head_commit_id, limit).await
    }

    pub async fn find_fork_of(
        &self,
        package_id: &str,
        original_repo_id: &str,
        forked_by: &str,
    ) -> Result<Option<(String, String)>> {
        self.queries
            .find_fork_of(package_id, original_repo_id, forked_by)
            .await
    }

    pub async fn list_pull_requests(
        &self,
        package_id: &str,
        repo_id: &str,
    ) -> Result<Vec<PullRequestRecord>> {
        self.queries.list_pull_requests(package_id, repo_id).await
    }

    pub async fn get_pull_request(&self, pr_id: &str) -> Result<PullRequestRecord> {
        self.queries.get_pull_request(pr_id).await
    }

    pub async fn get_initial_shared_version(&self, id: &str) -> Result<u64> {
        self.queries.get_initial_shared_version(id).await
    }

    // ─── Write operations via PTB ───────────────────────────────────────────

    async fn exec(
        &self,
        kp: &KeyPair,
        package_id: &str,
        module: &str,
        function: &str,
        args: Vec<Arg>,
    ) -> Result<ExecResult> {
        tx::execute_and_resolve_types(
            &self.gql_client,
            &self.graphql_url,
            kp,
            package_id,
            module,
            function,
            args,
            tx::DEFAULT_GAS_BUDGET,
        )
        .await
    }

    pub async fn create_repository(
        &self,
        kp: &KeyPair,
        package_id: &str,
        registry_id: &str,
        name: &str,
        description: &str,
        is_private: bool,
    ) -> Result<(String, String, GasCost)> {
        let registry_v = self.queries.get_initial_shared_version(registry_id).await?;
        let args = vec![
            Arg::shared(registry_id, registry_v, true),
            Arg::pure(&name.to_string())?,
            Arg::pure(&description.to_string())?,
            Arg::pure(&is_private)?,
            Arg::clock(),
        ];
        let result = self.exec(kp, package_id, "walgit", "create_repository", args).await?;
        let repo = result.find_created("::walgit::Repository").ok_or_else(|| {
            WalGitError::sui_transaction(format!(
                "could not identify Repository among created objects: [{}]",
                result.created_summary()
            ))
        })?;
        let acl = result.find_created("::walgit::AccessControl").ok_or_else(|| {
            WalGitError::sui_transaction(format!(
                "could not identify AccessControl among created objects: [{}]",
                result.created_summary()
            ))
        })?;
        Ok((repo.object_id.clone(), acl.object_id.clone(), result.gas))
    }

    pub async fn push_commit(
        &self,
        kp: &KeyPair,
        package_id: &str,
        repo_id: &str,
        acl_id: &str,
        blob_id: &str,
        git_head: &str,
        parent: Option<&str>,
        message: &str,
        branch: &str,
    ) -> Result<(String, GasCost)> {
        let repo_v = self.queries.get_initial_shared_version(repo_id).await?;
        let acl_v = self.queries.get_initial_shared_version(acl_id).await?;
        let parent_bytes: Vec<u8> = match parent {
            None => vec![],
            Some(p) => hex::decode(p.trim_start_matches("0x")).map_err(|_| {
                WalGitError::sui_transaction(format!("bad parent ID: {}", p))
            })?,
        };

        let args = vec![
            Arg::shared(repo_id, repo_v, true),
            Arg::shared(acl_id, acl_v, false),
            Arg::pure(&blob_id.to_string())?,
            Arg::pure(&git_head.to_string())?,
            Arg::pure(&parent_bytes)?,
            Arg::pure(&message.to_string())?,
            Arg::pure(&branch.to_string())?,
            Arg::clock(),
        ];
        let result = self.exec(kp, package_id, "walgit", "push_commit", args).await?;
        let commit = result
            .find_created("::walgit::Commit")
            .ok_or_else(|| WalGitError::sui_transaction("Commit object not created".to_string()))?;
        Ok((commit.object_id.clone(), result.gas))
    }

    pub async fn fork_repository(
        &self,
        kp: &KeyPair,
        package_id: &str,
        registry_id: &str,
        original_repo_id: &str,
        name: &str,
        description: &str,
    ) -> Result<(String, String, GasCost)> {
        let (registry_v, repo_v) = tokio::try_join!(
            self.queries.get_initial_shared_version(registry_id),
            self.queries.get_initial_shared_version(original_repo_id),
        )?;
        let args = vec![
            Arg::shared(registry_id, registry_v, true),
            Arg::shared(original_repo_id, repo_v, true),
            Arg::pure(&name.to_string())?,
            Arg::pure(&description.to_string())?,
            Arg::clock(),
        ];
        let result = self.exec(kp, package_id, "walgit", "fork_repository", args).await?;
        let repo = result
            .find_created("::walgit::Repository")
            .ok_or_else(|| WalGitError::sui_transaction("fork Repository not created".to_string()))?;
        let acl = result
            .find_created("::walgit::AccessControl")
            .ok_or_else(|| {
                WalGitError::sui_transaction("fork AccessControl not created".to_string())
            })?;
        Ok((repo.object_id.clone(), acl.object_id.clone(), result.gas))
    }

    pub async fn grant_access(
        &self,
        kp: &KeyPair,
        package_id: &str,
        acl_id: &str,
        address: &str,
        role: &str,
    ) -> Result<GasCost> {
        let func = match role {
            "read" => "grant_read_access",
            "write" => "grant_write_access",
            _ => return Err(WalGitError::other(format!("invalid role '{}'", role))),
        };
        let acl_v = self.queries.get_initial_shared_version(acl_id).await?;
        let target = sui_sdk_types::Address::from_hex(address)
            .map_err(|e| WalGitError::sui_transaction(format!("bad address: {}", e)))?;
        let args = vec![Arg::shared(acl_id, acl_v, true), Arg::Pure(bcs::to_bytes(&target)?)];
        let result = self.exec(kp, package_id, "walgit", func, args).await?;
        Ok(result.gas)
    }

    pub async fn revoke_access(
        &self,
        kp: &KeyPair,
        package_id: &str,
        acl_id: &str,
        address: &str,
        role: &str,
    ) -> Result<GasCost> {
        let func = match role {
            "read" => "revoke_read_access",
            "write" => "revoke_write_access",
            _ => return Err(WalGitError::other(format!("invalid role '{}'", role))),
        };
        let acl_v = self.queries.get_initial_shared_version(acl_id).await?;
        let target = sui_sdk_types::Address::from_hex(address)
            .map_err(|e| WalGitError::sui_transaction(format!("bad address: {}", e)))?;
        let args = vec![Arg::shared(acl_id, acl_v, true), Arg::Pure(bcs::to_bytes(&target)?)];
        let result = self.exec(kp, package_id, "walgit", func, args).await?;
        Ok(result.gas)
    }

    pub async fn create_pull_request(
        &self,
        kp: &KeyPair,
        package_id: &str,
        repo_id: &str,
        acl_id: &str,
        source_branch: &str,
        target_branch: &str,
        source_blob_id: &str,
    ) -> Result<(String, GasCost)> {
        let repo_v = self.queries.get_initial_shared_version(repo_id).await?;
        let acl_v = self.queries.get_initial_shared_version(acl_id).await?;
        let args = vec![
            Arg::shared(repo_id, repo_v, true),
            Arg::shared(acl_id, acl_v, false),
            Arg::pure(&source_branch.to_string())?,
            Arg::pure(&target_branch.to_string())?,
            Arg::pure(&source_blob_id.to_string())?,
            Arg::clock(),
        ];
        let result = self
            .exec(kp, package_id, "pull_request", "create_pull_request", args)
            .await?;
        let pr = result
            .find_created("::pull_request::PullRequest")
            .ok_or_else(|| WalGitError::sui_transaction("PullRequest not created".to_string()))?;
        Ok((pr.object_id.clone(), result.gas))
    }

    pub async fn approve_pull_request(
        &self,
        kp: &KeyPair,
        package_id: &str,
        pr_id: &str,
        repo_id: &str,
        acl_id: &str,
    ) -> Result<GasCost> {
        let pr_v = self.queries.get_initial_shared_version(pr_id).await?;
        let repo_v = self.queries.get_initial_shared_version(repo_id).await?;
        let acl_v = self.queries.get_initial_shared_version(acl_id).await?;
        let args = vec![
            Arg::shared(pr_id, pr_v, true),
            Arg::shared(repo_id, repo_v, false),
            Arg::shared(acl_id, acl_v, false),
            Arg::clock(),
        ];
        let result = self
            .exec(kp, package_id, "pull_request", "approve_pull_request", args)
            .await?;
        Ok(result.gas)
    }

    pub async fn merge_pull_request(
        &self,
        kp: &KeyPair,
        package_id: &str,
        pr_id: &str,
        repo_id: &str,
        acl_id: &str,
        merge_commit_blob_id: &str,
    ) -> Result<GasCost> {
        let pr_v = self.queries.get_initial_shared_version(pr_id).await?;
        let repo_v = self.queries.get_initial_shared_version(repo_id).await?;
        let acl_v = self.queries.get_initial_shared_version(acl_id).await?;
        let args = vec![
            Arg::shared(pr_id, pr_v, true),
            Arg::shared(repo_id, repo_v, false),
            Arg::shared(acl_id, acl_v, false),
            Arg::pure(&merge_commit_blob_id.to_string())?,
            Arg::clock(),
        ];
        let result = self
            .exec(kp, package_id, "pull_request", "merge_pull_request", args)
            .await?;
        Ok(result.gas)
    }

    pub async fn close_pull_request(
        &self,
        kp: &KeyPair,
        package_id: &str,
        pr_id: &str,
        repo_id: &str,
        acl_id: &str,
    ) -> Result<GasCost> {
        let pr_v = self.queries.get_initial_shared_version(pr_id).await?;
        let repo_v = self.queries.get_initial_shared_version(repo_id).await?;
        let acl_v = self.queries.get_initial_shared_version(acl_id).await?;
        let args = vec![
            Arg::shared(pr_id, pr_v, true),
            Arg::shared(repo_id, repo_v, false),
            Arg::shared(acl_id, acl_v, false),
            Arg::clock(),
        ];
        let result = self
            .exec(kp, package_id, "pull_request", "close_pull_request", args)
            .await?;
        Ok(result.gas)
    }

    pub fn graphql_url(&self) -> &str {
        &self.graphql_url
    }
}
