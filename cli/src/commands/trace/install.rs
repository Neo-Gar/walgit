// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

use super::helpers::{current_git_dir, current_repo_root};
use crate::error::{Result, WalGitError};
use crate::{hooks, trace_pending, ui};
use console::style;
use std::path::PathBuf;

// ─── agent registry ──────────────────────────────────────────────────────────

/// Status of a known agent adapter. `Planned` adapters are listed in the
/// picker so users can see the roadmap, but selecting one errors out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Available,
    Planned,
}

/// Static description of a supported agent adapter.
#[derive(Clone, Copy, Debug)]
pub struct AgentDef {
    /// Stable key accepted by `--agent <key>`. Lowercase, kebab-case.
    pub key: &'static str,
    /// Human-readable name shown in the picker and confirmation lines.
    pub label: &'static str,
    pub status: AgentStatus,
    /// Aliases accepted by `--agent` for ergonomics (e.g. `claude` → `claude-code`).
    pub aliases: &'static [&'static str],
}

/// Registry of supported agents. Order is the order shown in the picker.
/// Adding a new agent: append here, add a hook-installer arm in
/// [`install_one`] and [`uninstall_one`], flip status to `Available`.
pub const AGENTS: &[AgentDef] = &[
    AgentDef {
        key: "claude-code",
        label: "Claude Code",
        status: AgentStatus::Available,
        aliases: &["claude"],
    },
    AgentDef {
        key: "codex",
        label: "Codex (OpenAI)",
        status: AgentStatus::Planned,
        aliases: &[],
    },
    AgentDef {
        key: "cursor",
        label: "Cursor",
        status: AgentStatus::Planned,
        aliases: &[],
    },
    AgentDef {
        key: "gemini",
        label: "Gemini CLI",
        status: AgentStatus::Planned,
        aliases: &["gemini-cli"],
    },
    AgentDef {
        key: "copilot",
        label: "GitHub Copilot CLI",
        status: AgentStatus::Planned,
        aliases: &["copilot-cli", "gh-copilot"],
    },
    AgentDef {
        key: "factory",
        label: "Factory AI Droid",
        status: AgentStatus::Planned,
        aliases: &["factory-ai", "droid"],
    },
    AgentDef {
        key: "opencode",
        label: "OpenCode",
        status: AgentStatus::Planned,
        aliases: &[],
    },
];

// ─── scope ───────────────────────────────────────────────────────────────────

/// Where Claude Code hooks live. Both scopes are touched by default install
/// because Cursor's Claude Code extension reads only `Global`, while the
/// `claude` CLI in a terminal also picks up `Local` — and we want one
/// install command to cover both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    Local,
}

// ─── opts ─────────────────────────────────────────────────────────────────────

pub struct InstallOpts {
    pub agent_arg: Option<String>,
    pub no_global: bool,
    pub global_only: bool,
}

pub struct UninstallOpts {
    pub agent_arg: Option<String>,
    pub purge_global: bool,
}

// ─── resolve / picker ────────────────────────────────────────────────────────

/// Resolve a user-supplied `--agent` string to one or more known agents.
/// Accepts:
///   - `"all"` → every `Available` agent
///   - a comma-separated list of keys/aliases → those agents (any status)
///   - a single key/alias → that one agent
pub fn resolve_agent_arg(arg: &str) -> Result<Vec<&'static AgentDef>> {
    let arg = arg.trim();
    if arg.eq_ignore_ascii_case("all") {
        return Ok(AGENTS
            .iter()
            .filter(|a| a.status == AgentStatus::Available)
            .collect());
    }
    let mut out = Vec::new();
    for part in arg.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let found = AGENTS.iter().find(|a| {
            a.key.eq_ignore_ascii_case(part)
                || a.aliases.iter().any(|al| al.eq_ignore_ascii_case(part))
        });
        match found {
            Some(a) => out.push(a),
            None => {
                let keys: Vec<&str> = AGENTS.iter().map(|a| a.key).collect();
                return Err(WalGitError::other(format!(
                    "unknown agent '{}' (known: {})",
                    part,
                    keys.join(", ")
                )));
            }
        }
    }
    if out.is_empty() {
        return Err(WalGitError::other("empty --agent value".to_string()));
    }
    Ok(out)
}

/// Interactive multi-select picker. Available agents are pre-checked;
/// planned ones are listed unchecked so users can see the roadmap. Falls
/// back to all Available adapters when stdin is not a TTY (so CI / scripts
/// behave deterministically).
fn pick_agents_interactive() -> Result<Vec<&'static AgentDef>> {
    use console::Term;
    use dialoguer::{MultiSelect, theme::ColorfulTheme};

    let stdout_tty = Term::stdout().is_term();
    let stdin_tty = Term::stderr().is_term(); // stderr is a decent proxy when piping
    if !stdout_tty || !stdin_tty {
        // Non-interactive: install everything we have an adapter for.
        return Ok(AGENTS
            .iter()
            .filter(|a| a.status == AgentStatus::Available)
            .collect());
    }

    // The picker only includes Available agents so users can't pick a
    // not-yet-implemented one and end up with a confusing "planned, skipped"
    // warning. Planned agents are listed separately above the picker so the
    // roadmap is still visible without being selectable.
    let available: Vec<&AgentDef> = AGENTS
        .iter()
        .filter(|a| a.status == AgentStatus::Available)
        .collect();
    let planned: Vec<&AgentDef> = AGENTS
        .iter()
        .filter(|a| a.status == AgentStatus::Planned)
        .collect();

    if !planned.is_empty() {
        let names: Vec<&str> = planned.iter().map(|a| a.label).collect();
        println!(
            "  {} {}: {}",
            style("·").cyan(),
            style("planned (not yet selectable)").dim(),
            style(names.join(", ")).dim(),
        );
    }

    // dialoguer doesn't render its own key-hint line, so print one ourselves.
    // Without this, first-time users hit Enter immediately and get whatever
    // defaults we chose for them — usually fine but surprising.
    println!(
        "{} {}",
        style("?").yellow().bold(),
        style("↑/↓ move · space toggle · enter confirm · esc cancel").dim(),
    );

    // Resolve installation status per agent so the picker can flag what's
    // already there. We default these entries to *unchecked* so a user who
    // hits enter without thinking doesn't trigger pointless re-installs.
    // "Installed" here means present in either scope (global or local), so
    // a user who only ever installed globally still gets the badge.
    let installed: Vec<bool> = available.iter().map(|a| agent_is_installed(a)).collect();

    let items: Vec<String> = available
        .iter()
        .zip(&installed)
        .map(|(a, &inst)| {
            if inst {
                format!("{}  (already installed)", a.label)
            } else {
                a.label.to_string()
            }
        })
        .collect();
    let defaults: Vec<bool> = installed.iter().map(|&i| !i).collect();

    let chosen = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Which agents should walgit record traces for?")
        .items(&items)
        .defaults(&defaults)
        .interact_opt()
        .map_err(|e| WalGitError::other(format!("picker failed: {}", e)))?;

    let chosen = chosen.ok_or_else(|| WalGitError::other("install cancelled".to_string()))?;
    let selected: Vec<&'static AgentDef> = chosen.iter().map(|&i| available[i]).collect();

    // If any selection would re-install an already-installed adapter, ask
    // for a single Y/n confirmation listing them. Default Yes — the user
    // explicitly checked the box, this is just a tripwire against fat-finger.
    let to_reinstall: Vec<&str> = chosen
        .iter()
        .filter(|&&i| installed[i])
        .map(|&i| available[i].label)
        .collect();
    if !to_reinstall.is_empty() {
        let question = format!("re-install {}?", to_reinstall.join(", "));
        let confirm = ui::prompt_yes_no(&question, true)
            .map_err(|e| WalGitError::other(format!("confirmation failed: {}", e)))?;
        if !confirm {
            // Drop the already-installed ones; keep the rest of the selection.
            return Ok(chosen
                .into_iter()
                .filter(|&i| !installed[i])
                .map(|i| available[i])
                .collect());
        }
    }

    Ok(selected)
}

// ─── install / uninstall ─────────────────────────────────────────────────────

pub async fn install(opts: InstallOpts) -> Result<()> {
    let agents = match opts.agent_arg {
        Some(s) => resolve_agent_arg(&s)?,
        None => pick_agents_interactive()?,
    };
    if agents.is_empty() {
        ui::warn("no agents selected — nothing to install");
        return Ok(());
    }

    let write_global = !opts.no_global;
    let write_local = !opts.global_only;

    // Git hook + opt-in marker live per-repo.
    let git_dir = current_git_dir().ok();
    match &git_dir {
        Some(g) => {
            let hook_path = hooks::install_git_hook(g)?;
            ui::success(format!(
                "git hook installed: {}",
                ui::dim(&hook_path.display().to_string())
            ));
            trace_pending::mark_enabled(g)?;
            ui::success(format!(
                "marker written: {}",
                ui::dim(&trace_pending::enabled_path(g).display().to_string())
            ));
        }
        None => {
            ui::warn(
                "not inside a git repository — skipping git hook and opt-in marker (Claude Code hooks still go to user-global if --no-global isn't set)",
            );
        }
    }

    let mut planned_skipped = Vec::new();
    for a in &agents {
        if a.status == AgentStatus::Planned {
            planned_skipped.push(a.label);
            continue;
        }
        if write_global {
            install_one(a, Scope::Global)?;
        }
        if write_local && git_dir.is_some() {
            install_one(a, Scope::Local)?;
        }
    }

    for label in planned_skipped {
        ui::warn(format!("{} adapter is not implemented yet", label));
    }

    // Agents read their settings once at session start, not on every tool call.
    // Any session that was open during install won't see the new hooks until restarted.
    let installed_labels: Vec<&str> = agents
        .iter()
        .filter(|a| a.status == AgentStatus::Available)
        .map(|a| a.label)
        .collect();
    if !installed_labels.is_empty() {
        println!();
        ui::warn(format!(
            "restart any open {} session(s) for hooks to take effect — \
             agents read settings only at session start",
            installed_labels.join(", ")
        ));
    }

    Ok(())
}

pub async fn uninstall(opts: UninstallOpts) -> Result<()> {
    let agents = match opts.agent_arg {
        Some(s) => resolve_agent_arg(&s)?,
        // For uninstall, default to ALL known agents (Planned too — uninstall
        // is a no-op for never-installed adapters and we want a clean
        // sweep when the user just runs `walgit trace uninstall`).
        None => AGENTS.iter().collect(),
    };

    if let Ok(git_dir) = current_git_dir() {
        if hooks::uninstall_git_hook(&git_dir)? {
            ui::success("git hook removed");
        }
        if trace_pending::is_enabled(&git_dir) {
            trace_pending::unmark_enabled(&git_dir)?;
            ui::success("opt-in marker removed");
        }
    }
    for a in &agents {
        uninstall_one(a, Scope::Local)?;
        if opts.purge_global {
            uninstall_one(a, Scope::Global)?;
        }
    }
    if !opts.purge_global {
        ui::info(
            "user-global Claude Code hooks left in place — they're gated by per-repo markers and harmless. Pass --purge-global to remove them too.",
        );
    }
    Ok(())
}

// ─── per-agent dispatch ───────────────────────────────────────────────────────

/// Best-effort check whether an adapter is installed in ANY scope. Used to
/// decorate the picker; not a hard guarantee.
fn agent_is_installed(a: &AgentDef) -> bool {
    match a.key {
        "claude-code" => {
            let g = claude_settings_path(Scope::Global)
                .map(|p| hooks::is_claude_installed(&p))
                .unwrap_or(false);
            let l = claude_settings_path(Scope::Local)
                .map(|p| hooks::is_claude_installed(&p))
                .unwrap_or(false);
            g || l
        }
        _ => false,
    }
}

/// Per-agent install dispatch. New adapters get an arm here.
fn install_one(a: &AgentDef, scope: Scope) -> Result<()> {
    match a.key {
        "claude-code" => {
            let settings = claude_settings_path(scope)?;
            // Global hooks must be gated so they don't fire in non-walgit
            // repos; local ones don't need gating because the project is
            // itself the opt-in signal.
            let gated = scope == Scope::Global;
            hooks::install_claude_settings(&settings, gated)?;
            ui::success(format!(
                "Claude Code hooks installed ({:?}): {}",
                scope,
                ui::dim(&settings.display().to_string())
            ));
            Ok(())
        }
        other => Err(WalGitError::other(format!(
            "adapter '{}' has no installer wired up yet — please file an issue",
            other
        ))),
    }
}

/// Per-agent uninstall dispatch. Tolerant: missing files are not an error.
fn uninstall_one(a: &AgentDef, scope: Scope) -> Result<()> {
    match a.key {
        "claude-code" => {
            let settings = claude_settings_path(scope)?;
            if hooks::uninstall_claude_settings(&settings)? {
                ui::success(format!(
                    "Claude Code hooks removed ({:?}): {}",
                    scope,
                    ui::dim(&settings.display().to_string())
                ));
            }
            Ok(())
        }
        _ => Ok(()), // planned adapters have nothing to remove yet
    }
}

fn claude_settings_path(scope: Scope) -> Result<PathBuf> {
    match scope {
        Scope::Global => {
            let home = dirs::home_dir()
                .ok_or_else(|| WalGitError::other("cannot resolve home directory"))?;
            Ok(home.join(".claude").join("settings.json"))
        }
        Scope::Local => {
            // Project-local: <repo-root>/.claude/settings.json. Falls back to CWD
            // if not inside a git repo (caller already warned).
            let root = current_repo_root().unwrap_or_else(|_| std::env::current_dir().unwrap());
            Ok(root.join(".claude").join("settings.json"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_agent_arg_all_returns_only_available() {
        let v = resolve_agent_arg("all").unwrap();
        assert!(v.iter().all(|a| a.status == AgentStatus::Available));
        assert!(v.iter().any(|a| a.key == "claude-code"));
        assert!(!v.iter().any(|a| a.key == "codex")); // planned, excluded
    }

    #[test]
    fn resolve_agent_arg_accepts_alias() {
        let v = resolve_agent_arg("claude").unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].key, "claude-code");
    }

    #[test]
    fn resolve_agent_arg_accepts_comma_list() {
        let v = resolve_agent_arg("claude-code,codex").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].key, "claude-code");
        assert_eq!(v[1].key, "codex");
    }

    #[test]
    fn resolve_agent_arg_rejects_unknown() {
        let e = resolve_agent_arg("emacs-doctor").unwrap_err();
        assert!(format!("{}", e).contains("unknown agent"));
    }
}
