// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! `walgit-mcp` — Model Context Protocol server exposing the WalGit CLI as
//! agent-callable tools over stdio JSON-RPC. The server itself talks only to
//! the MCP client and to the `walgit` binary as a subprocess; it doesn't
//! touch Sui or Walrus directly.

mod protocol;
mod tools;

use anyhow::Result;
use protocol::{
    Capabilities, Content, InitializeResult, Request, Response, ServerInfo, ToolCallResult,
    errors,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() -> Result<()> {
    let walgit_bin = tools::resolve_walgit_binary()?;
    eprintln!(
        "walgit-mcp: started, walgit binary = {}",
        walgit_bin.display()
    );

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break; // EOF, client disconnected
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(req) => handle_request(&walgit_bin, req).await,
            Err(e) => Some(Response::err(
                Value::Null,
                errors::PARSE_ERROR,
                format!("parse error: {}", e),
            )),
        };

        if let Some(resp) = response {
            let mut s = serde_json::to_string(&resp)?;
            s.push('\n');
            stdout.write_all(s.as_bytes()).await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

/// Returns `Some(Response)` for requests, `None` for notifications.
async fn handle_request(walgit_bin: &PathBuf, req: Request) -> Option<Response> {
    let is_notification = req.id.is_none();
    let id = req.id.clone().unwrap_or(Value::Null);

    let result = match req.method.as_str() {
        "initialize" => Ok(json!(InitializeResult {
            protocol_version: protocol::PROTOCOL_VERSION,
            capabilities: Capabilities { tools: json!({}) },
            server_info: ServerInfo {
                name: "walgit-mcp",
                version: env!("CARGO_PKG_VERSION"),
            },
        })),

        "notifications/initialized" => {
            // Client done with handshake. No response, just continue.
            return None;
        }

        "ping" => Ok(json!({})),

        "tools/list" => Ok(json!({ "tools": tools::list_tools() })),

        "tools/call" => match req.params.get("name").and_then(Value::as_str) {
            Some(name) => {
                let args = req
                    .params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                match tools::dispatch(walgit_bin, name, &args).await {
                    Ok(result) => Ok(serde_json::to_value(&result).unwrap_or(Value::Null)),
                    Err(e) => {
                        // Surface tool errors as a structured ToolCallResult with
                        // isError=true rather than a JSON-RPC error — agents
                        // typically handle the former more gracefully.
                        let result = ToolCallResult {
                            content: vec![Content::Text {
                                text: format!("walgit tool error: {}", e),
                            }],
                            is_error: true,
                        };
                        Ok(serde_json::to_value(&result).unwrap_or(Value::Null))
                    }
                }
            }
            None => Err((errors::INVALID_PARAMS, "missing 'name'".to_string())),
        },

        other => Err((
            errors::METHOD_NOT_FOUND,
            format!("method not found: {}", other),
        )),
    };

    if is_notification {
        // Per JSON-RPC 2.0, notifications get no response even on error.
        return None;
    }

    Some(match result {
        Ok(v) => Response::ok(id, v),
        Err((code, msg)) => Response::err(id, code, msg),
    })
}

// Silence the unused-import warning since `errors::INTERNAL_ERROR` is reserved
// for future use without breaking the public constant list.
#[allow(dead_code)]
const _: i64 = errors::INTERNAL_ERROR;
