// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Reasoning trace — structured JSON an agent attaches to a commit describing
//! *what it did and why*. Stored inside the git commit message so it's part
//! of the SHA (tamper-evident) and visible to every git-aware tool.
//!
//! See `.agents/TRACE_SCHEMA.md` for the schema spec.

use crate::error::{Result, WalGitError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Footer marker. Must appear on its own line. The trace JSON occupies every
/// line after this marker until EOF.
pub const TRACE_MARKER: &str = "--- walgit-trace ---";

/// Soft cap (4 KB) — agents should summarise tool I/O above this.
pub const TRACE_SOFT_CAP_BYTES: usize = 4 * 1024;
/// Hard cap (16 KB) — refuse to commit beyond this.
pub const TRACE_HARD_CAP_BYTES: usize = 16 * 1024;

/// Schema v0 of a reasoning trace.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Trace {
    /// Schema version, currently always `"1"`.
    pub version: String,

    pub agent_id: String,
    pub run_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,

    pub task: String,

    #[serde(default)]
    pub tools_called: Vec<ToolCall>,

    pub decision: String,

    #[serde(default)]
    pub alternatives_considered: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,

    /// Forward-compatible extension bucket. Unknown keys live here so we can
    /// extend the spec without bumping `version`.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub extensions: Value,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub input_summary: String,
    pub output_summary: String,
}

impl Trace {
    /// Parse the trace JSON and validate it conforms to schema v0.
    pub fn parse(json: &str) -> Result<Self> {
        let trace: Trace = serde_json::from_str(json)
            .map_err(|e| WalGitError::other(format!("trace JSON parse: {}", e)))?;
        trace.validate()?;
        Ok(trace)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != "1" {
            return Err(WalGitError::other(format!(
                "unsupported trace version '{}' (expected '1')",
                self.version
            )));
        }
        if self.agent_id.trim().is_empty() {
            return Err(WalGitError::other("trace.agent_id is empty".to_string()));
        }
        if self.run_id.trim().is_empty() {
            return Err(WalGitError::other("trace.run_id is empty".to_string()));
        }
        if self.task.trim().is_empty() {
            return Err(WalGitError::other("trace.task is empty".to_string()));
        }
        if self.task.len() > 200 {
            return Err(WalGitError::other(format!(
                "trace.task too long ({} > 200 chars)",
                self.task.len()
            )));
        }
        if let Some(c) = self.confidence {
            if !(0.0..=1.0).contains(&c) {
                return Err(WalGitError::other(format!(
                    "trace.confidence out of range: {} (must be in [0,1])",
                    c
                )));
            }
        }
        Ok(())
    }

    /// Pretty-printed JSON for embedding in commit messages.
    pub fn to_pretty_json(&self) -> Result<String> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| WalGitError::other(format!("trace serialize: {}", e)))?;
        if json.len() > TRACE_HARD_CAP_BYTES {
            return Err(WalGitError::other(format!(
                "trace too large ({} bytes > {} hard cap) — summarize tool I/O",
                json.len(),
                TRACE_HARD_CAP_BYTES
            )));
        }
        Ok(json)
    }

    /// Suggest summarisation when the trace exceeds the soft cap. Returns
    /// `None` if under cap, `Some(msg)` to surface as a warning.
    pub fn soft_cap_warning(&self) -> Option<String> {
        let n = serde_json::to_string(self).ok()?.len();
        if n > TRACE_SOFT_CAP_BYTES {
            Some(format!(
                "trace is {} bytes (soft cap {} KB) — consider shortening tool summaries",
                n,
                TRACE_SOFT_CAP_BYTES / 1024
            ))
        } else {
            None
        }
    }
}

/// Append a trace footer to an existing commit message body.
/// Idempotent: if a trace block is already present, it is replaced.
pub fn attach_to_message(message: &str, trace: &Trace) -> Result<String> {
    let json = trace.to_pretty_json()?;
    let stripped = strip_trace_block(message);
    let body = stripped.trim_end_matches('\n');
    let separator = if body.is_empty() { "" } else { "\n\n" };
    Ok(format!("{}{}{}\n{}\n", body, separator, TRACE_MARKER, json))
}

/// Extract the trace JSON from a commit message body, if present.
/// Returns `Ok(Some(trace))` on success, `Ok(None)` when no trace block is
/// present, `Err` when a block is present but malformed.
pub fn extract_from_message(message: &str) -> Result<Option<Trace>> {
    let Some(json) = extract_trace_json(message) else {
        return Ok(None);
    };
    Ok(Some(Trace::parse(&json)?))
}

/// Return only the message portion without the trace footer.
pub fn strip_trace_block(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    for line in message.lines() {
        if line == TRACE_MARKER {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Raw JSON extraction — used both by `extract_from_message` and by tools
/// that want to render the JSON without validating (`walgit show --trace`
/// should still display malformed traces with a warning rather than hide
/// them entirely).
pub fn extract_trace_json(message: &str) -> Option<String> {
    let mut in_block = false;
    let mut buf = String::new();
    for line in message.lines() {
        if !in_block {
            if line == TRACE_MARKER {
                in_block = true;
            }
            continue;
        }
        buf.push_str(line);
        buf.push('\n');
    }
    if in_block {
        Some(buf.trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Trace {
        Trace {
            version: "1".into(),
            agent_id: "writer-v1".into(),
            run_id: "01J-test".into(),
            parent_run_id: None,
            task: "add a getter".into(),
            tools_called: vec![ToolCall {
                name: "read_file".into(),
                input_summary: "src/lib.rs".into(),
                output_summary: "42 lines".into(),
            }],
            decision: "wrote the getter".into(),
            alternatives_considered: vec!["could have inlined it".into()],
            confidence: Some(0.9),
            extensions: Value::Null,
        }
    }

    #[test]
    fn round_trip_attach_extract() {
        let trace = sample();
        let msg = attach_to_message("feat: getter\n\ncool change", &trace).unwrap();
        let back = extract_from_message(&msg).unwrap().unwrap();
        assert_eq!(back.agent_id, "writer-v1");
        assert_eq!(back.tools_called.len(), 1);
    }

    #[test]
    fn extract_returns_none_for_plain_commit() {
        let msg = "just a regular commit\n\nno trace here";
        assert!(extract_from_message(msg).unwrap().is_none());
    }

    #[test]
    fn strip_removes_trace_block() {
        let trace = sample();
        let msg = attach_to_message("feat: getter\n\nbody", &trace).unwrap();
        let plain = strip_trace_block(&msg);
        assert!(!plain.contains(TRACE_MARKER));
        assert!(plain.contains("feat: getter"));
        assert!(plain.contains("body"));
    }

    #[test]
    fn attach_is_idempotent() {
        let trace = sample();
        let once = attach_to_message("feat: x", &trace).unwrap();
        let twice = attach_to_message(&once, &trace).unwrap();
        assert_eq!(twice.matches(TRACE_MARKER).count(), 1);
    }

    #[test]
    fn rejects_unknown_version() {
        let bad = r#"{"version":"99","agent_id":"a","run_id":"r","task":"t","tools_called":[],"decision":"d","alternatives_considered":[]}"#;
        assert!(Trace::parse(bad).is_err());
    }

    #[test]
    fn rejects_oversized_task() {
        let mut t = sample();
        t.task = "x".repeat(300);
        assert!(t.validate().is_err());
    }

    #[test]
    fn rejects_out_of_range_confidence() {
        let mut t = sample();
        t.confidence = Some(2.0);
        assert!(t.validate().is_err());
    }

    #[test]
    fn malformed_json_returns_err() {
        let msg = "subject\n\n--- walgit-trace ---\nnot json at all\n";
        assert!(extract_from_message(msg).is_err());
    }

    #[test]
    fn marker_with_leading_whitespace_is_ignored() {
        // Marker line must be exact; this is part of the body, not a marker.
        let msg = "subject\n\n  --- walgit-trace ---\n{...}\n";
        assert!(extract_from_message(msg).unwrap().is_none());
    }
}
