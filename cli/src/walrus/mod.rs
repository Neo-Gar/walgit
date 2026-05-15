// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Walrus blob storage client with streaming uploads and retry/backoff.

mod client;

pub use client::{UploadResult, WalrusClient, estimate_cost_usd};
