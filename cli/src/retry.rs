// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Exponential-backoff retry helper for transient network errors.

use crate::error::{Result, WalGitError};
use std::future::Future;
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub multiplier: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_delay_ms: 250,
            max_delay_ms: 4_000,
            multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    pub fn aggressive() -> Self {
        Self {
            max_attempts: 6,
            initial_delay_ms: 200,
            max_delay_ms: 8_000,
            multiplier: 2.0,
        }
    }

    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            initial_delay_ms: 0,
            max_delay_ms: 0,
            multiplier: 1.0,
        }
    }
}

/// Run an async operation with exponential backoff. Only retries on transient
/// errors (`WalGitError::is_transient()`). The closure is called fresh on each
/// attempt so it can build new futures/connections.
pub async fn with_backoff<T, Fut, F>(policy: RetryPolicy, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut delay_ms = policy.initial_delay_ms;
    let mut last_err: Option<WalGitError> = None;

    for attempt in 1..=policy.max_attempts {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !e.is_transient() || attempt == policy.max_attempts {
                    return Err(e);
                }
                eprintln!(
                    "walgit: transient error (attempt {}/{}): {} — retrying in {}ms",
                    attempt, policy.max_attempts, e, delay_ms
                );
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = ((delay_ms as f64) * policy.multiplier) as u64;
                if delay_ms > policy.max_delay_ms {
                    delay_ms = policy.max_delay_ms;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| WalGitError::other("retry loop exhausted")))
}
