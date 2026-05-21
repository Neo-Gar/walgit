// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

/// Pull request module for WalGit.
///
/// A PullRequest is a shared object that tracks a proposed merge from
/// `source_branch` into `target_branch` within a single Repository.
/// Lifecycle: OPEN → MERGED (via merge_pull_request) or CLOSED (via close_pull_request).
///
/// PR numbers are allocated by Repository's monotonic counter (walgit::next_pr_number),
/// so concurrent creates never collide.
module walgit::pull_request;

use std::string::String;
use sui::clock::Clock;
use sui::event;
use walgit::walgit::{
    Repository,
    AccessControl,
    get_acl_id,
    get_acl_repo_id,
    is_private,
    get_allowed_readers,
    get_allowed_writers,
    get_repo_owner,
    next_pr_number,
};

// ─── Error codes ──────────────────────────────────────────────────────────────
const ENotWriter: u64 = 0;
const ENotOwner: u64 = 1;
const ENotApproved: u64 = 2;
const EAlreadyMerged: u64 = 3;
const EAlreadyClosed: u64 = 4;
const EWrongRepo: u64 = 5;
const ESelfApprove: u64 = 6;
const ENoAccess: u64 = 7;
const EAclMismatch: u64 = 8;

// ─── Status constants ─────────────────────────────────────────────────────────
const STATUS_OPEN: u8 = 0;
const STATUS_MERGED: u8 = 1;
const STATUS_CLOSED: u8 = 2;

// ─── Events ───────────────────────────────────────────────────────────────────

public struct PRCreated has copy, drop {
    pr_id: ID,
    repo_id: ID,
    number: u64,
    author: address,
    source_branch: String,
    target_branch: String,
    source_blob_id: String,
    source_git_head: String,
    created_at: u64,
}

public struct PRApproved has copy, drop {
    pr_id: ID,
    repo_id: ID,
    approved_by: address,
    approved_at: u64,
}

public struct PRMerged has copy, drop {
    pr_id: ID,
    repo_id: ID,
    merged_by: address,
    merge_commit_blob_id: String,
    merged_at: u64,
}

public struct PRClosed has copy, drop {
    pr_id: ID,
    repo_id: ID,
    closed_by: address,
    closed_at: u64,
}

// ─── Struct ───────────────────────────────────────────────────────────────────

public struct PullRequest has key {
    id: UID,
    repo_id: ID,
    /// Sequential PR number within the repository (#1, #2, …)
    number: u64,
    author: address,
    source_branch: String,
    target_branch: String,
    /// Walrus blob_id of the packfile containing the proposed changes
    source_blob_id: String,
    /// Git SHA1 (40-char hex) of the PR's tip commit. Lets the maintainer
    /// run `git merge --ff-only <sha>` after unpacking the Walrus blob —
    /// without this the dangling commit can't be addressed by any ref.
    source_git_head: String,
    status: u8,
    approved: bool,
    approved_by: Option<address>,
    approved_at: Option<u64>,
    merge_commit_blob_id: Option<String>,
    merged_by: Option<address>,
    merged_at: Option<u64>,
    created_at: u64,
}

// ─── Pull request lifecycle ───────────────────────────────────────────────────

/// Open a new pull request.
///
/// Access rules:
/// - Public repo  → anyone may open a PR.
/// - Private repo → only the owner, a listed reader, or a listed writer may open a PR.
///
/// The ACL must be the one bound to `repo`; passing a foreign ACL aborts with EAclMismatch.
public fun create_pull_request(
    repo: &mut Repository,
    acl: &AccessControl,
    source_branch: String,
    target_branch: String,
    source_blob_id: String,
    source_git_head: String,
    clock: &Clock,
    ctx: &mut TxContext,
) {
    assert!(object::id(acl) == get_acl_id(repo), EAclMismatch);

    let sender = ctx.sender();
    if (is_private(repo)) {
        assert!(
            sender == get_repo_owner(repo) ||
            vector::contains(get_allowed_readers(acl), &sender) ||
            vector::contains(get_allowed_writers(acl), &sender),
            ENoAccess,
        );
    };

    let repo_id = object::id(repo);
    let now = clock.timestamp_ms();
    let number = next_pr_number(repo);

    let pr = PullRequest {
        id: object::new(ctx),
        repo_id,
        number,
        author: sender,
        source_branch,
        target_branch,
        source_blob_id,
        source_git_head,
        status: STATUS_OPEN,
        approved: false,
        approved_by: option::none(),
        approved_at: option::none(),
        merge_commit_blob_id: option::none(),
        merged_by: option::none(),
        merged_at: option::none(),
        created_at: now,
    };

    event::emit(PRCreated {
        pr_id: object::id(&pr),
        repo_id,
        number,
        author: sender,
        source_branch: pr.source_branch,
        target_branch: pr.target_branch,
        source_blob_id: pr.source_blob_id,
        source_git_head: pr.source_git_head,
        created_at: now,
    });

    transfer::share_object(pr);
}

/// Approve a pull request.
/// Only the repository owner or a listed writer may approve.
/// Self-approval (author approving their own PR) is forbidden.
public fun approve_pull_request(
    pr: &mut PullRequest,
    repo: &Repository,
    acl: &AccessControl,
    clock: &Clock,
    ctx: &TxContext,
) {
    assert!(pr.repo_id == object::id(repo), EWrongRepo);
    assert!(object::id(acl) == get_acl_id(repo), EAclMismatch);
    assert!(get_acl_repo_id(acl) == pr.repo_id, EAclMismatch);
    // Separate checks so the caller receives the correct error code.
    assert!(pr.status != STATUS_MERGED, EAlreadyMerged);
    assert!(pr.status != STATUS_CLOSED, EAlreadyClosed);

    let sender = ctx.sender();
    assert!(
        sender == get_repo_owner(repo) ||
        vector::contains(get_allowed_writers(acl), &sender),
        ENotWriter,
    );
    assert!(sender != pr.author, ESelfApprove);

    let now = clock.timestamp_ms();
    pr.approved = true;
    pr.approved_by = option::some(sender);
    pr.approved_at = option::some(now);

    event::emit(PRApproved {
        pr_id: object::id(pr),
        repo_id: pr.repo_id,
        approved_by: sender,
        approved_at: now,
    });
}

/// Merge a pull request.
/// Requires prior approval. Only the repository owner or a listed writer may merge.
///
/// The corresponding merge commit on the target branch must be created via
/// walgit::push_commit in the same PTB — the two calls are atomic.
public fun merge_pull_request(
    pr: &mut PullRequest,
    repo: &Repository,
    acl: &AccessControl,
    merge_commit_blob_id: String,
    clock: &Clock,
    ctx: &TxContext,
) {
    assert!(pr.repo_id == object::id(repo), EWrongRepo);
    assert!(object::id(acl) == get_acl_id(repo), EAclMismatch);
    assert!(pr.status != STATUS_MERGED, EAlreadyMerged);
    assert!(pr.status != STATUS_CLOSED, EAlreadyClosed);
    assert!(pr.approved, ENotApproved);

    let sender = ctx.sender();
    assert!(
        sender == get_repo_owner(repo) ||
        vector::contains(get_allowed_writers(acl), &sender),
        ENotWriter,
    );

    let now = clock.timestamp_ms();
    pr.status = STATUS_MERGED;
    pr.merge_commit_blob_id = option::some(merge_commit_blob_id);
    pr.merged_by = option::some(sender);
    pr.merged_at = option::some(now);

    event::emit(PRMerged {
        pr_id: object::id(pr),
        repo_id: pr.repo_id,
        merged_by: sender,
        merge_commit_blob_id: *option::borrow(&pr.merge_commit_blob_id),
        merged_at: now,
    });
}

/// Close a pull request without merging.
/// Only the repository owner or the PR author may close.
public fun close_pull_request(
    pr: &mut PullRequest,
    repo: &Repository,
    acl: &AccessControl,
    clock: &Clock,
    ctx: &TxContext,
) {
    assert!(pr.repo_id == object::id(repo), EWrongRepo);
    assert!(object::id(acl) == get_acl_id(repo), EAclMismatch);
    assert!(pr.status != STATUS_MERGED, EAlreadyMerged);
    assert!(pr.status != STATUS_CLOSED, EAlreadyClosed);

    let sender = ctx.sender();
    assert!(
        sender == get_repo_owner(repo) || sender == pr.author,
        ENotOwner,
    );

    pr.status = STATUS_CLOSED;

    event::emit(PRClosed {
        pr_id: object::id(pr),
        repo_id: pr.repo_id,
        closed_by: sender,
        closed_at: clock.timestamp_ms(),
    });
}

// ─── View functions ────────────────────────────────────────────────────────────

public fun get_pr_status(pr: &PullRequest): u8 { pr.status }

public fun get_pr_author(pr: &PullRequest): address { pr.author }

public fun get_pr_approved(pr: &PullRequest): bool { pr.approved }

public fun get_pr_approved_by(pr: &PullRequest): Option<address> { pr.approved_by }

public fun get_pr_source_branch(pr: &PullRequest): String { pr.source_branch }

public fun get_pr_target_branch(pr: &PullRequest): String { pr.target_branch }

public fun get_pr_source_blob_id(pr: &PullRequest): String { pr.source_blob_id }

public fun get_pr_source_git_head(pr: &PullRequest): String { pr.source_git_head }

public fun get_pr_number(pr: &PullRequest): u64 { pr.number }

public fun get_pr_merge_blob_id(pr: &PullRequest): Option<String> { pr.merge_commit_blob_id }

public fun get_pr_repo_id(pr: &PullRequest): ID { pr.repo_id }
