// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! `walgit trace diff <sha_a> <sha_b>` — side-by-side reasoning diff. Anchor
//! use case: regression debugging ("what changed in the agent's reasoning
//! between the good commit and the bad one?").

use crate::commands::find_repo;
use crate::error::{Result, WalGitError};
use crate::trace::{self, Trace};
use crate::{git, ui};
use console::style;
use std::collections::HashSet;

pub async fn diff(sha_a: String, sha_b: String) -> Result<()> {
    let (repo_dir, _walgit_dir, _local) = find_repo()?;
    let a = load(&repo_dir, &sha_a)?;
    let b = load(&repo_dir, &sha_b)?;

    ui::header(&format!(
        "trace diff: {} → {}",
        ui::short_hash(&a.sha),
        ui::short_hash(&b.sha)
    ));

    diff_str("agent_id", &a.trace.agent_id, &b.trace.agent_id);
    diff_str("task", &a.trace.task, &b.trace.task);
    diff_optional_str(
        "parent_run_id",
        a.trace.parent_run_id.as_deref(),
        b.trace.parent_run_id.as_deref(),
    );

    println!();
    diff_tools(&a.trace, &b.trace);

    println!();
    diff_str_block("decision", &a.trace.decision, &b.trace.decision);

    println!();
    diff_string_set(
        "alternatives_considered",
        &a.trace.alternatives_considered,
        &b.trace.alternatives_considered,
    );

    println!();
    diff_optional_f32("confidence", a.trace.confidence, b.trace.confidence);

    Ok(())
}

struct Loaded {
    sha: String,
    trace: Trace,
}

fn load(repo_dir: &std::path::Path, sha: &str) -> Result<Loaded> {
    let full_sha = git::rev_parse(repo_dir, sha)?;
    let msg = git::read_commit_message(repo_dir, &full_sha)?;
    let Some(json) = trace::extract_trace_json(&msg) else {
        return Err(WalGitError::other(format!(
            "commit {} has no reasoning trace",
            full_sha
        )));
    };
    let trace = Trace::parse(&json)?;
    Ok(Loaded {
        sha: full_sha,
        trace,
    })
}

fn diff_str(field: &str, a: &str, b: &str) {
    if a == b {
        println!(
            "  {} {} {}",
            style("=").dim(),
            ui::label(&format!("{:<16}", field)),
            ui::dim(a),
        );
    } else {
        println!(
            "  {} {} {} → {}",
            style("~").yellow().bold(),
            ui::label(&format!("{:<16}", field)),
            style(a).red(),
            style(b).green(),
        );
    }
}

fn diff_optional_str(field: &str, a: Option<&str>, b: Option<&str>) {
    let a = a.unwrap_or("(none)");
    let b = b.unwrap_or("(none)");
    diff_str(field, a, b);
}

fn diff_optional_f32(field: &str, a: Option<f32>, b: Option<f32>) {
    let a_s = a.map(|n| format!("{:.2}", n)).unwrap_or("(none)".into());
    let b_s = b.map(|n| format!("{:.2}", n)).unwrap_or("(none)".into());
    diff_str(field, &a_s, &b_s);
}

fn diff_str_block(field: &str, a: &str, b: &str) {
    println!("  {}", ui::label(field));
    if a == b {
        for line in a.lines() {
            println!("    {} {}", style("=").dim(), ui::dim(line));
        }
    } else {
        for line in a.lines() {
            println!("    {} {}", style("-").red().bold(), style(line).red());
        }
        for line in b.lines() {
            println!("    {} {}", style("+").green().bold(), style(line).green());
        }
    }
}

fn diff_tools(a: &Trace, b: &Trace) {
    let names_a: Vec<&str> = a.tools_called.iter().map(|t| t.name.as_str()).collect();
    let names_b: Vec<&str> = b.tools_called.iter().map(|t| t.name.as_str()).collect();
    println!("  {}", ui::label("tools_called"));
    if names_a == names_b {
        for tc in &a.tools_called {
            println!(
                "    {} {} {}",
                style("=").dim(),
                ui::highlight(&tc.name),
                ui::dim(&tc.input_summary),
            );
        }
    } else {
        let set_a: HashSet<&str> = names_a.iter().copied().collect();
        let set_b: HashSet<&str> = names_b.iter().copied().collect();
        for tc in &a.tools_called {
            let marker = if set_b.contains(tc.name.as_str()) {
                style("=").dim().to_string()
            } else {
                style("-").red().bold().to_string()
            };
            println!(
                "    {} {} {}",
                marker,
                style(&tc.name).red(),
                ui::dim(&tc.input_summary),
            );
        }
        for tc in &b.tools_called {
            if set_a.contains(tc.name.as_str()) {
                continue;
            }
            println!(
                "    {} {} {}",
                style("+").green().bold(),
                style(&tc.name).green(),
                ui::dim(&tc.input_summary),
            );
        }
    }
}

fn diff_string_set(field: &str, a: &[String], b: &[String]) {
    let set_a: HashSet<&String> = a.iter().collect();
    let set_b: HashSet<&String> = b.iter().collect();
    println!("  {}", ui::label(field));
    if set_a == set_b {
        if a.is_empty() {
            println!("    {}", ui::dim("(empty)"));
        } else {
            for v in a {
                println!("    {} {}", style("=").dim(), ui::dim(v));
            }
        }
    } else {
        for v in a {
            let marker = if set_b.contains(v) {
                style("=").dim().to_string()
            } else {
                style("-").red().bold().to_string()
            };
            println!("    {} {}", marker, style(v).red());
        }
        for v in b {
            if set_a.contains(v) {
                continue;
            }
            println!("    {} {}", style("+").green().bold(), style(v).green());
        }
    }
}
