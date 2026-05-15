// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Sui blockchain client — reads via GraphQL, writes via native PTB + JSON-RPC.

pub mod client;
pub mod keystore;
pub mod queries;
pub mod tx;
pub mod types;

pub use client::SuiClient;
pub use types::{
    AccessRecord, CommitRecord, GasCost, PullRequestRecord, RepoRecord, seal_id,
};
