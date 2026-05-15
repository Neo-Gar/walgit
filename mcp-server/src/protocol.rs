// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Minimal JSON-RPC 2.0 + MCP message types. We hand-roll these rather than
//! pulling in `rmcp` so the dependency surface stays tiny and any protocol
//! quirk is patchable in a single file.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP protocol version we advertise. Bumping this only when we test against
/// a newer client.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Deserialize, Debug)]
pub struct Request {
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// Notifications omit `id`; requests carry it.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Serialize, Debug)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

#[derive(Serialize, Debug)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ─── MCP-specific payloads ────────────────────────────────────────────────────

#[derive(Serialize, Debug)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    pub capabilities: Capabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Serialize, Debug)]
pub struct Capabilities {
    pub tools: Value,
}

#[derive(Serialize, Debug)]
pub struct ServerInfo {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Serialize, Debug)]
pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Serialize, Debug)]
pub struct ToolCallResult {
    pub content: Vec<Content>,
    #[serde(rename = "isError", skip_serializing_if = "is_false")]
    pub is_error: bool,
}

#[derive(Serialize, Debug)]
#[serde(tag = "type")]
pub enum Content {
    #[serde(rename = "text")]
    Text { text: String },
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Standard JSON-RPC error codes.
pub mod errors {
    pub const PARSE_ERROR: i64 = -32700;
    #[allow(dead_code)]
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}
