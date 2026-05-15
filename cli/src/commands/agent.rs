// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

//! Agent-facing helpers. Today: `walgit agent commit` — staged files + git
//! commit with a reasoning trace footer embedded into the commit message.

use crate::commands::find_repo;
use crate::error::{Result, WalGitError};
use crate::trace::{Trace, attach_to_message};
use crate::{git, ui};
use std::io::Read;
use std::path::Path;

pub async fn commit(paths: Vec<String>, message: String, trace_arg: String) -> Result<()> {
    let (repo_dir, _walgit_dir, _local) = find_repo()?;

    let trace = load_trace(&trace_arg)?;
    if let Some(warning) = trace.soft_cap_warning() {
        ui::warn(warning);
    }

    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    git::add(&repo_dir, &path_refs)?;

    if !git::has_staged_changes(&repo_dir) {
        return Err(WalGitError::other(
            "nothing staged after `git add` — paths matched no changes".to_string(),
        ));
    }

    let full_message = attach_to_message(&message, &trace)?;
    git::commit_with_long_message(&repo_dir, &full_message)?;

    let sha = git::get_head_commit(&repo_dir)?;
    ui::success(format!(
        "{} {} ({})",
        ui::short_hash(&sha),
        ui::highlight(&trace.task),
        ui::dim(&trace.agent_id),
    ));
    if let Some(parent) = &trace.parent_run_id {
        ui::info(format!("parent_run_id: {}", parent));
    }
    Ok(())
}

fn load_trace(arg: &str) -> Result<Trace> {
    let raw = if arg == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        std::fs::read_to_string(Path::new(arg))?
    };
    Trace::parse(&raw)
}
