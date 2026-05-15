// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! `walgit show [<commit>] [--trace]` — print a single commit. With `--trace`,
//! render the embedded reasoning trace footer if present.

use crate::commands::find_repo;
use crate::error::{Result, WalGitError};
use crate::trace::{self, Trace};
use crate::{git, ui};
use console::style;

pub async fn run(commit: String, show_trace: bool) -> Result<()> {
    let (repo_dir, _walgit_dir, _local) = find_repo()?;
    let sha = git::rev_parse(&repo_dir, &commit)?;
    let full = git::read_commit_message(&repo_dir, &sha)?;

    let plain = trace::strip_trace_block(&full);
    let plain = plain.trim_end();

    ui::header(&format!("commit {}", ui::short_hash(&sha)));
    for (i, line) in plain.lines().enumerate() {
        if i == 0 {
            println!("  {}", ui::highlight(line));
        } else if line.is_empty() {
            println!();
        } else {
            println!("  {}", line);
        }
    }

    if !show_trace {
        return Ok(());
    }

    let Some(raw_json) = trace::extract_trace_json(&full) else {
        ui::header("trace");
        ui::info("this commit has no reasoning trace");
        return Ok(());
    };

    ui::header("trace");
    match Trace::parse(&raw_json) {
        Ok(t) => render_trace(&t),
        Err(e) => {
            ui::warn(format!("trace block present but unparseable: {}", e));
            println!();
            println!("{}", ui::dim(&raw_json));
        }
    }
    Ok(())
}

pub fn render_trace(t: &Trace) {
    println!(
        "  {} {} {}",
        ui::label("agent   "),
        ui::highlight(&t.agent_id),
        ui::dim(&format!("(run {})", short_run(&t.run_id))),
    );
    if let Some(parent) = &t.parent_run_id {
        println!(
            "  {} {}",
            ui::label("parent  "),
            ui::dim(&short_run(parent))
        );
    }
    println!("  {} {}", ui::label("task    "), ui::highlight(&t.task));
    if let Some(c) = t.confidence {
        let painted = if c >= 0.7 {
            style(format!("{:.2}", c)).green()
        } else if c >= 0.4 {
            style(format!("{:.2}", c)).yellow()
        } else {
            style(format!("{:.2}", c)).red()
        };
        println!("  {} {}", ui::label("confid. "), painted);
    }
    println!();
    if !t.tools_called.is_empty() {
        println!("  {}", ui::label("tools called"));
        for call in &t.tools_called {
            println!(
                "    {} {} {} → {}",
                ui::dim("·"),
                ui::highlight(&call.name),
                ui::dim(&call.input_summary),
                ui::dim(&call.output_summary),
            );
        }
        println!();
    }
    println!("  {}", ui::label("decision"));
    for line in t.decision.lines() {
        println!("    {}", line);
    }
    if !t.alternatives_considered.is_empty() {
        println!();
        println!("  {}", ui::label("alternatives considered"));
        for alt in &t.alternatives_considered {
            println!("    {} {}", ui::dim("✗"), alt);
        }
    }
}

fn short_run(id: &str) -> String {
    if id.len() > 12 {
        format!("{}…", &id[..12])
    } else {
        id.to_string()
    }
}

// Surface a typed error so callers can match if needed.
#[allow(dead_code)]
fn _typed_err() -> WalGitError {
    WalGitError::other("placeholder".to_string())
}
