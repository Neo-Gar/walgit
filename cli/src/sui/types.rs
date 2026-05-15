// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct GasCost {
    pub computation_mist: u64,
    pub storage_mist: u64,
    pub rebate_mist: u64,
}

impl GasCost {
    pub fn net_mist(&self) -> u64 {
        let gross = self.computation_mist + self.storage_mist;
        gross.saturating_sub(self.rebate_mist)
    }

    pub fn net_sui(&self) -> f64 {
        self.net_mist() as f64 / 1_000_000_000.0
    }

    pub fn display(&self) -> String {
        format!(
            "{:.6} SUI  (compute: {} + storage: {} − rebate: {} MIST)",
            self.net_sui(),
            self.computation_mist,
            self.storage_mist,
            self.rebate_mist,
        )
    }
}

impl std::ops::Add for GasCost {
    type Output = GasCost;
    fn add(self, rhs: GasCost) -> GasCost {
        GasCost {
            computation_mist: self.computation_mist + rhs.computation_mist,
            storage_mist: self.storage_mist + rhs.storage_mist,
            rebate_mist: self.rebate_mist + rhs.rebate_mist,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommitRecord {
    pub id: String,
    pub blob_id: String,
    pub git_head: String,
    pub parent: Option<String>,
    pub message: String,
    pub author: String,
    pub timestamp: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RepoRecord {
    pub id: String,
    pub acl_id: String,
    pub owner: String,
    pub name: String,
    pub is_private: bool,
    pub branches: HashMap<String, String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AccessRecord {
    pub acl_id: String,
    pub owner: String,
    pub allowed_readers: Vec<String>,
    pub allowed_writers: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PullRequestRecord {
    pub id: String,
    pub repo_id: String,
    pub number: u64,
    pub author: String,
    pub source_branch: String,
    pub target_branch: String,
    pub source_blob_id: String,
    /// 0 = open, 1 = merged, 2 = closed
    pub status: u8,
    pub approved: bool,
    pub approved_by: Option<String>,
    pub merge_commit_blob_id: Option<String>,
    pub merged_by: Option<String>,
    pub created_at: u64,
}

impl PullRequestRecord {
    pub fn status_label(&self) -> &'static str {
        match self.status {
            1 => "merged",
            2 => "closed",
            _ => "open",
        }
    }
}

/// Seal IBE identity for a repository: `package_id_bytes ++ repo_id_bytes` (64 bytes).
pub fn seal_id(package_id: &str, repo_id: &str) -> Vec<u8> {
    let pkg = hex::decode(package_id.trim_start_matches("0x")).unwrap_or_default();
    let repo = hex::decode(repo_id.trim_start_matches("0x")).unwrap_or_default();
    [pkg, repo].concat()
}
