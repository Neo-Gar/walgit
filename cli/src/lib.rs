// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! WalGit core library: shared by the `walgit` CLI binary and the
//! `git-remote-walgit` helper.

pub mod cli;
pub mod commands;
pub mod config;
pub mod error;
pub mod git;
pub mod hooks;
pub mod memwal;
pub mod retry;
pub mod seal;
pub mod sui;
pub mod trace;
pub mod trace_pending;
pub mod ui;
pub mod validate;
pub mod walrus;

pub use config::{Config, LocalRepoConfig, PushRecord};
pub use error::{Result, WalGitError};
pub use seal::SealClient;
pub use sui::SuiClient;
pub use walrus::WalrusClient;
