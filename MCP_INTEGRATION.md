# Using WalGit from an AI Agent

`walgit-mcp` is a [Model Context Protocol](https://modelcontextprotocol.io/)
server. It speaks JSON-RPC 2.0 over stdio and exposes WalGit's CLI as a set
of agent-callable tools.

## Prerequisites

- `walgit` binary on `PATH` (or pointed to via `WALGIT_BIN`)
- `git` ≥ 2.30 (the `walgit` binary will refuse to run with anything older)
- A configured `~/.walgit/config.toml` with `package_id` and `registry_id` set

## Configure once

Build both binaries from a workspace clone:

```bash
cargo install --path cli
cargo install --path mcp-server
```

This drops `walgit` and `walgit-mcp` into `~/.cargo/bin/`.

## Plug into Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json`:

```jsonc
{
  "mcpServers": {
    "walgit": {
      "command": "walgit-mcp",
      "env": {
        // Optional — defaults to `walgit` on PATH.
        "WALGIT_BIN": "/Users/you/.cargo/bin/walgit"
      }
    }
  }
}
```

Restart Claude Desktop. The 14 `walgit_*` tools appear in the tool drawer.

## Plug into Claude Agent SDK

```python
from claude_agent_sdk import Agent, McpServer

agent = Agent(
    mcp_servers=[
        McpServer(
            name="walgit",
            command="walgit-mcp",
            env={"WALGIT_BIN": "/Users/you/.cargo/bin/walgit"},
        )
    ],
)
```

## Tool inventory

| Name                | Purpose                                                            |
| ------------------- | ------------------------------------------------------------------ |
| `walgit_init`       | Create a new repo on chain + locally                               |
| `walgit_fork`       | Fork a public repo into the agent's address                        |
| `walgit_status`     | Show repo metadata                                                 |
| `walgit_log`        | Recent commits (with optional trace summary)                       |
| `walgit_show`       | Single commit with optional trace render                           |
| `walgit_agent_commit` | Stage + commit with a reasoning-trace footer                     |
| `walgit_pr_create`  | Open a PR (defaults to fork → upstream)                            |
| `walgit_pr_show`    | PR metadata                                                        |
| `walgit_pr_diff`    | PR diff against target                                             |
| `walgit_pr_approve` | Approve a PR                                                       |
| `walgit_pr_merge`   | Merge an approved PR                                               |
| `walgit_pr_close`   | Close without merging                                              |
| `walgit_pr_list`    | List PRs (`mine=true` for cross-repo)                              |
| `walgit_trace_diff` | Side-by-side reasoning diff of two commits                         |

Every tool accepts an optional `cwd` argument so the agent can operate on
multiple working directories within the same session.

## Reasoning trace contract

`walgit_agent_commit` requires a `trace` object matching schema v0 (see
[.agents/TRACE_SCHEMA.md](TRACE_SCHEMA.md)). Minimal example payload:

```json
{
  "name": "walgit_agent_commit",
  "arguments": {
    "cwd": "/path/to/repo",
    "paths": ["src/handler.rs"],
    "message": "feat: add rate limit",
    "trace": {
      "version": "1",
      "agent_id": "writer-v1",
      "run_id": "01J-...",
      "task": "add rate limit to /api/users",
      "tools_called": [
        { "name": "read_file",  "input_summary": "src/handler.rs",
          "output_summary": "412 lines" }
      ],
      "decision": "added tower-http RateLimitLayer with 60rpm bucket",
      "alternatives_considered": [
        "in-memory HashMap (rejected: per-instance state)"
      ]
    }
  }
}
```

The trace is embedded into the git commit message footer
(`--- walgit-trace ---` block), making it part of the commit SHA — tamper-
evident and visible to every downstream tool. **Do not put secrets into
traces; commits propagate to Walrus and are world-readable.**

## Manual debugging

Send raw JSON-RPC frames over stdio to inspect tool output:

```bash
( echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}'
  echo '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  echo '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  echo '{"jsonrpc":"2.0","id":3,"method":"tools/call",
         "params":{"name":"walgit_status","arguments":{"cwd":"."}}}'
) | walgit-mcp | jq .
```

## Operational notes

- The MCP server is purely a stdio proxy. It does not touch Sui or Walrus
  itself; every chain interaction happens inside the `walgit` binary it
  spawns, which uses the user's Sui keystore at `~/.sui/sui_config/sui.keystore`.
- ANSI color codes are stripped from tool output so agent context windows
  stay clean.
- Errors from `walgit` are returned as a `ToolCallResult` with `isError: true`
  rather than as JSON-RPC errors, so agents can read and react to them
  conversationally.
