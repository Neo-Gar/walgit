// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use crate::error::{Result, WalGitError};
use crate::retry::{RetryPolicy, with_backoff};
use crate::ui;
use indicatif::ProgressBar;
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct WalrusClient {
    publisher_url: Arc<str>,
    aggregator_url: Arc<str>,
    http: Client,
    retry: RetryPolicy,
}

#[derive(Deserialize, Debug)]
struct UploadResponse {
    #[serde(rename = "newlyCreated")]
    newly_created: Option<NewlyCreated>,
    #[serde(rename = "alreadyCertified")]
    already_certified: Option<AlreadyCertified>,
}

#[derive(Deserialize, Debug)]
struct NewlyCreated {
    #[serde(rename = "blobObject")]
    blob_object: BlobObject,
}

#[derive(Deserialize, Debug)]
struct BlobObject {
    #[serde(rename = "blobId")]
    blob_id: String,
}

#[derive(Deserialize, Debug)]
struct AlreadyCertified {
    #[serde(rename = "blobId")]
    blob_id: String,
}

pub struct UploadResult {
    pub blob_id: String,
    /// True when Walrus already had this exact content — no storage charge.
    pub rebate: bool,
}

impl WalrusClient {
    pub fn new(publisher_url: String, aggregator_url: String) -> Self {
        Self::with_retry(publisher_url, aggregator_url, RetryPolicy::default())
    }

    pub fn with_retry(
        publisher_url: String,
        aggregator_url: String,
        retry: RetryPolicy,
    ) -> Self {
        let http = Client::builder()
            // Walrus uploads can be slow on large blobs and during epoch transitions.
            .timeout(Duration::from_secs(180))
            .build()
            .expect("reqwest client build failed");
        Self {
            publisher_url: publisher_url.into(),
            aggregator_url: aggregator_url.into(),
            http,
            retry,
        }
    }

    /// Upload binary data to Walrus for `epochs` storage epochs.
    /// Returns `UploadResult { blob_id, rebate }`.
    pub async fn upload(&self, data: Vec<u8>, epochs: u32) -> Result<UploadResult> {
        let size = data.len();
        let url = format!("{}/v1/blobs?epochs={}", self.publisher_url, epochs);

        let pb = ui::spinner(format!(
            "Uploading {} to Walrus ({} epoch{})…",
            ui::fmt_bytes(size),
            epochs,
            if epochs == 1 { "" } else { "s" }
        ));

        let data = Arc::new(data);
        let result = {
            let http = self.http.clone();
            let url = url.clone();
            let data = Arc::clone(&data);
            with_backoff(self.retry, move || {
                let http = http.clone();
                let url = url.clone();
                let data = Arc::clone(&data);
                async move { perform_upload(&http, &url, &data).await }
            })
            .await
        };

        match result {
            Ok(out) => {
                report_upload(&pb, &out, size, epochs);
                Ok(out)
            }
            Err(e) => {
                pb.finish_and_clear();
                Err(e)
            }
        }
    }

    /// Download a blob from Walrus by its blob_id.
    pub async fn download(&self, blob_id: &str) -> Result<Vec<u8>> {
        let url = format!("{}/v1/blobs/{}", self.aggregator_url, blob_id);
        let http = self.http.clone();
        let blob_id = blob_id.to_string();
        with_backoff(self.retry, move || {
            let http = http.clone();
            let url = url.clone();
            let blob_id = blob_id.clone();
            async move { perform_download(&http, &url, &blob_id).await }
        })
        .await
    }
}

async fn perform_upload(http: &Client, url: &str, data: &[u8]) -> Result<UploadResult> {
    let response = http
        .put(url)
        .header("Content-Type", "application/octet-stream")
        .body(data.to_vec())
        .send()
        .await
        .map_err(|e| WalGitError::WalrusUpload(format!("connect failed: {}", e)))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        if status.as_u16() == 402 {
            return Err(WalGitError::InsufficientWal);
        }
        return Err(WalGitError::WalrusUpload(format!(
            "HTTP {}: {}",
            status, body
        )));
    }

    let parsed: UploadResponse = response
        .json()
        .await
        .map_err(|e| WalGitError::WalrusUpload(format!("parse failed: {}", e)))?;

    if let Some(newly) = parsed.newly_created {
        Ok(UploadResult {
            blob_id: newly.blob_object.blob_id,
            rebate: false,
        })
    } else if let Some(cert) = parsed.already_certified {
        Ok(UploadResult {
            blob_id: cert.blob_id,
            rebate: true,
        })
    } else {
        Err(WalGitError::WalrusUpload(
            "unexpected response: missing blob_id".to_string(),
        ))
    }
}

async fn perform_download(http: &Client, url: &str, blob_id: &str) -> Result<Vec<u8>> {
    let response = http.get(url).send().await.map_err(|e| {
        WalGitError::WalrusDownload {
            blob_id: blob_id.to_string(),
            reason: format!("connect failed: {}", e),
        }
    })?;

    if !response.status().is_success() {
        return Err(WalGitError::WalrusDownload {
            blob_id: blob_id.to_string(),
            reason: format!("HTTP {}", response.status()),
        });
    }

    let bytes = response.bytes().await.map_err(|e| WalGitError::WalrusDownload {
        blob_id: blob_id.to_string(),
        reason: format!("body read failed: {}", e),
    })?;
    Ok(bytes.to_vec())
}

fn report_upload(pb: &ProgressBar, out: &UploadResult, size: usize, epochs: u32) {
    // Clear the spinner first so its line doesn't collide with the success
    // message — `finish_with_message` leaves residue that bleeds into the
    // next eprintln when called from contexts like git-remote-walgit.
    pb.finish_and_clear();
    let short = &out.blob_id[..12.min(out.blob_id.len())];
    if out.rebate {
        ui::esuccess(format!(
            "blob {} already on Walrus — storage rebate (no charge)",
            short
        ));
    } else {
        ui::esuccess(format!(
            "uploaded {} → {} ({} epoch{})",
            ui::fmt_bytes(size),
            short,
            epochs,
            if epochs == 1 { "" } else { "s" }
        ));
    }
}

/// Rough Walrus cost estimate (storage is subsidised; minimum billable blob is ~64 MB).
pub fn estimate_cost_usd(bytes: usize, epochs: u32) -> f64 {
    const RATE_USD_PER_TB_PER_YEAR: f64 = 50.0;
    const EPOCHS_PER_YEAR: f64 = 52.0;
    const MIN_BILLABLE_BYTES: f64 = 64.0 * 1_024.0 * 1_024.0;
    let effective = (bytes as f64).max(MIN_BILLABLE_BYTES);
    let tb = effective / 1e12;
    tb * RATE_USD_PER_TB_PER_YEAR * (epochs as f64 / EPOCHS_PER_YEAR)
}
