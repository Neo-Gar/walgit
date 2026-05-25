# WalGit

**Git for the agent-native era.**

WalGit is a decentralized Git host built on [Sui](https://sui.io) smart contracts and
[Walrus](https://walrus.xyz) blob storage. You push, pull, and clone with the same `git`
commands you already use — but every commit becomes on-chain provenance, every private repo
is end-to-end encrypted with [Seal](https://github.com/MystenLabs/seal), and every commit can
carry a **reasoning trace** that turns the repository into a searchable, tamper-evident memory
of *why* the code is the way it is.

No company owns your code. Ownership is your Sui wallet.

```
Sui  ·  Walrus  ·  Seal  ·  Rust CLI  ·  MCP-ready
```

---

## Why WalGit

- **Works with native git.** `git push`, `git pull`, `git clone` — unchanged. A
  `git-remote-walgit` helper transparently handles the `walgit://` URL scheme.
- **On-chain provenance.** Each commit is a Sui object linked to its Walrus blob. Forks, ACL
  changes, and PRs are all auditable on chain.
- **Sealed private repos.** Private repositories use Seal identity-based encryption gated by an
  on-chain ACL. Only addresses on the read list can derive a decryption key — no key rotation
  on grant.
- **🧠 Repository Memory (the feature we lead with).** Reasoning traces attached to commits make
  the repo a per-project memory cell — searchable by meaning, tamper-evident, agent-readable.
  [Jump to Repository Memory ↓](#-repository-memory)

---

## Repository layout

This is a Cargo workspace:

| Crate | Binary | Role |
|-------|--------|------|
| [`cli/`](cli) | `walgit` | Core library + the main CLI (Sui, Walrus, Seal, git, config, traces) |
| [`git-remote/`](git-remote) | `git-remote-walgit` | Thin remote helper that registers the `walgit://` URL scheme for native git |
| [`mcp-server/`](mcp-server) | `walgit-mcp` | MCP server exposing 14 WalGit tools to agents (Claude Desktop, Agent SDK) |
| [`contracts/`](contracts) | — | Sui Move contracts (`Registry`, `Repository`, `Commit`, `AccessControl`, `PullRequest`) |

Git objects are packed via native `git` subprocess; Sui writes go through native PTBs (no Sui
CLI dependency for transactions).

---

## Install

```bash
curl -sSfL https://raw.githubusercontent.com/Neo-Gar/walgit/main/install.sh | sh
```

The installer:

1. Detects your platform and downloads the `walgit`, `git-remote-walgit`, and `walgit-mcp`
   binaries (with checksum verification) into `~/.local/bin`.
2. Checks for `git` (required — WalGit wraps it).
3. Installs [`betterleaks`](https://github.com/betterleaks/betterleaks) for secret scanning
   (via `brew` or `dnf`; runs before every push, PR, and MemWal upload).
4. Installs the Sui CLI (via [`suiup`](https://github.com/Mystenlabs/suiup)) and Walrus CLI,
   and offers to create a Sui wallet for you.

### Environment variables

| Variable | Default | Meaning |
|----------|---------|---------|
| `WALGIT_VERSION` | `latest` | Release tag to install |
| `WALGIT_PREFIX` | `$HOME/.local/bin` | Install location for binaries |
| `WALGIT_NETWORK` | `testnet` | Sui/Walrus network |
| `WALGIT_SKIP_SUI` | `0` | Skip suiup/sui install |
| `WALGIT_SKIP_WAL` | `0` | Skip walrus install |
| `WALGIT_SKIP_BETTERLEAKS` | `0` | Skip betterleaks install |

### Build from source

```bash
git clone https://github.com/Neo-Gar/walgit
cd walgit
cargo build --release            # binaries land in target/release/
# or install directly:
cargo install --path cli
cargo install --path git-remote
cargo install --path mcp-server
```

---

## First-time setup

After installing, three things must be in place before `walgit init` works:

**1. Point the CLI at the deployed WalGit package (required).** Every on-chain command needs the
package and registry object IDs for your network:

```bash
walgit config --package-id <PACKAGE_ID> --registry-id <REGISTRY_ID>
```

**2. A funded Sui wallet.** The installer offers to create one
(`sui client new-address ed25519 walgit-account`); on testnet, fund it from the faucet so you can
pay gas.

**3. `~/.local/bin` on your `PATH`** (the installer prints the line to add if it isn't).

Check everything with:

```bash
walgit config --show
walgit status
```

---

## Quickstart

```bash
# Create a repo (registers it on Sui, wires up the `origin` remote)
walgit init my-project
cd my-project

# Work with plain git
echo "# my-project" > README.md
git add README.md
git commit -m "init"

# Push — git-remote-walgit packs objects, stores them on Walrus,
# and records the commit on Sui
git push -u origin main
```

Clone from anywhere:

```bash
git clone walgit://<owner-address>/my-project
```

Private repos add Seal encryption automatically:

```bash
walgit init secret-project --private
walgit access grant read 0x<collaborator-address>   # a single Sui tx
```

---

## 🧠 Repository Memory

> Code remembers *why*, not just *what*.

This is the feature WalGit leads with. Every commit can carry a **reasoning trace** — the task,
the decision made, the alternatives rejected, the tools used, and a self-rated confidence. Traces
live in three places at once:

- **In the commit message** (inside a `--- walgit-trace ---` fence) — so Git's SHA seals them.
  Any edit changes the SHA.
- **In `.git/walgit/traces/`** — local snapshots for offline recall.
- **In [MemWal](#memwal)** — an off-chain relayer that vector-embeds traces for semantic search
  across the repo's entire history.

Multiply this across a repo's lifetime and you get **one memory cell per repository** —
project-scoped (not user-scoped), portable across forks, permissioned by the same Seal ACL as the
code, and survivable if the relayer ever disappears.

### Why it matters

| Audience | What they get |
|----------|---------------|
| **Future-you** | `walgit trace recall "cache layer"` surfaces the original decision and the alternatives considered. |
| **Agents** | LLMs are stateless; repository memory gives them long-term, per-project continuity — including alternatives they already rejected. |
| **Teams** | When someone leaves, their reasoning stays. Onboarding becomes `walgit trace recall "auth module"` instead of "ping whoever wrote this." |
| **Auditors** | Every decision around sensitive code is timestamped on chain and cryptographically sealed in Git. Replays and retroactive edits are detectable. |

### Set it up

```bash
# Per machine: create a MemWal account on the web, then configure the delegate key.
#   Mainnet  → https://memwal.ai
#   Testnet  → https://staging.memwal.ai
# The web app generates a delegate keypair and registers it on-chain. Then:
walgit memwal init

# Per repo: install hooks so traces record automatically as you work
walgit trace install --agent claude-code
```

### Use it

```bash
# Start a trace for a work session (hooks also do this automatically)
walgit trace start --task "add a token-bucket rate limiter"

# ... do the work; Claude Code / Cursor hooks append prompts and tool calls ...

# Record the decision before committing
walgit trace set --decision "in-memory token bucket, 30 req/min per IP" \
                 --alternative "redis sliding window — extra dep, deferred"

git commit -m "feat: rate limiter"     # post-commit hook seals the trace to the SHA

walgit trace upload                    # ship snapshots to MemWal
walgit trace recall "rate limiting"    # semantic search across the repo's history
walgit show <sha> --trace              # read any commit's full reasoning
walgit trace diff <sha_a> <sha_b>      # compare reasoning side-by-side
```

> **Don't put secrets in traces.** Hooks redact common patterns and `betterleaks` scans the
> pending trace before snapshot — but assume any tool input you wouldn't want a future reader to
> see shouldn't be fed to an agent in the first place.

---

## Command reference

| Area | Commands |
|------|----------|
| **Repo** | `walgit init <name> [--here] [--private] [--epochs N]`, `walgit status`, `walgit log [--traces]`, `walgit show [<sha>] [--trace]` |
| **Access** | `walgit access list`, `walgit access grant <read\|write> <addr> [--memwal-pubkey <hex>]`, `walgit access revoke …` |
| **Forks & PRs** | `walgit fork <walgit://owner/repo>`, `walgit pr create`, `walgit pr list [--mine]`, `walgit pr show\|diff\|approve\|merge\|close <id>` |
| **Memory** | `walgit trace start\|record\|set\|status\|abort\|snapshot\|upload\|recall\|diff\|install\|uninstall`, `walgit memwal init\|status\|list\|add-delegate\|remove-delegate` |
| **Agents** | `walgit agent commit <paths> -m <msg> --trace <file>` |
| **Maintenance** | `walgit cache list\|clean`, `walgit config [--network …] [--package-id …] [--show]` |

Run `walgit --help` or `walgit <command> --help` for full flags.

---

## MCP integration

`walgit-mcp` exposes 14 tools (`walgit_init`, `walgit_agent_commit`, `walgit_pr_*`,
`walgit_trace_diff`, …) so agents can drive WalGit directly. See
[MCP_INTEGRATION.md](MCP_INTEGRATION.md) for wiring it into Claude Desktop or the Claude Agent SDK.

---

## Architecture in one diagram

```
  ┌──────────┐   git push   ┌─────────────────────┐
  │  git CLI │ ───────────▶ │ git-remote-walgit   │
  └──────────┘              └─────────┬───────────┘
                                      │ 1. pack objects
                                      │ 2. (Seal-encrypt if private)
                                      ▼
                            ┌─────────────────────┐
                            │      Walrus         │  ← bytes (blob_id)
                            └─────────┬───────────┘
                                      │ blob_id
                                      ▼
                            ┌─────────────────────┐
                            │       Sui           │  ← Commit object
                            │  Repository.branch  │     + ACL check
                            └─────────────────────┘
```

- **Sui** holds structured metadata and access logic (atomic, auditable).
- **Walrus** holds the bytes (cheap immutable storage with retention epochs).
- **Seal** encrypts those bytes so the ACL enforces confidentiality, not just trust.

---

## License

Apache-2.0. See [LICENSE](LICENSE).
