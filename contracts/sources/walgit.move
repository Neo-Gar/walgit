// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

/// WalGit — decentralized Git hosting on Walrus + Sui.
///
/// Storage layout:
///   Repository    — shared object; holds branch → Commit ID table.
///   AccessControl — shared object linked from Repository; holds the
///                   allowed-readers / allowed-writers lists and exposes
///                   the `seal_approve` entry point for Seal key servers.
///   Commit        — shared object; holds Walrus blob_id + git HEAD SHA.
module walgit::walgit;

use std::string::String;
use sui::clock::Clock;
use sui::event;
use sui::table::{Self, Table};
use sui::vec_set::{Self, VecSet};

// ─── Error codes ──────────────────────────────────────────────────────────────
const ENotOwner: u64 = 0;
const ENoAccess: u64 = 1;
const ESelfFork: u64 = 2;
const EAlreadyForked: u64 = 3;
const EAclMismatch: u64 = 4;

// ─── Events ───────────────────────────────────────────────────────────────────

public struct RepositoryCreated has copy, drop {
    repo_id: ID,
    acl_id: ID,
    owner: address,
    name: String,
    description: String,
    is_private: bool,
    created_at: u64,
}

public struct CommitPushed has copy, drop {
    repo_id: ID,
    commit_id: ID,
    branch_name: String,
    git_head: String,
    blob_id: String,
    author: address,
    timestamp: u64,
}

public struct RepositoryDeleted has copy, drop {
    repo_id: ID,
    owner: address,
}

public struct RepositoryForked has copy, drop {
    original_repo_id: ID,
    fork_repo_id: ID,
    fork_acl_id: ID,
    forked_by: address,
    fork_name: String,
    forked_at: u64,
}

public struct AccessChanged has copy, drop {
    acl_id: ID,
    repo_id: ID,
    address: address,
    /// 0 = read, 1 = write
    role: u8,
    /// true = granted, false = revoked
    granted: bool,
}

// ─── Structs ──────────────────────────────────────────────────────────────────

public struct Repository has key {
    id: UID,
    owner: address,
    name: String,
    description: String,
    is_private: bool,
    /// branch name → Commit object ID
    branches: Table<String, ID>,
    created_at: u64,
    /// ID of the companion shared AccessControl object
    acl_id: ID,
    /// Set of addresses that have forked this repository.
    /// Enforces one-fork-per-address at the contract level.
    forked_by: VecSet<address>,
    /// Monotonic counter for PR numbers; incremented by pull_request::create_pull_request.
    /// Replaces the previous "Unix timestamp as PR number" scheme which broke under
    /// concurrent creates.
    next_pr_number: u64,
}

/// Shared object — holds per-repository access control.
/// `repo_id` binds this ACL exclusively to one Repository so that
/// seal_approve and push_commit cannot be called with a mismatched ACL.
public struct AccessControl has key {
    id: UID,
    /// Address of the repository owner (only they can mutate this object)
    owner: address,
    /// The Repository object this ACL is bound to — verified in seal_approve and push_commit.
    repo_id: ID,
    /// Addresses allowed to decrypt private-repo content via Seal
    allowed_readers: vector<address>,
    /// Addresses allowed to push commits (enforced by push_commit)
    allowed_writers: vector<address>,
}

public struct Commit has key {
    id: UID,
    repo_id: ID,
    /// Walrus blob ID (base58-encoded string)
    blob_id: String,
    /// Git commit SHA of HEAD at push time (40-char hex string)
    git_head: String,
    /// Parent Commit object ID, or none for the initial commit
    parent: Option<ID>,
    message: String,
    author: address,
    /// Milliseconds since Unix epoch (Sui Clock)
    timestamp: u64,
}

// ─── Seal integration ─────────────────────────────────────────────────────────

/// Extract bytes [start, end) from a vector<u8>.
fun bytes_slice(v: &vector<u8>, start: u64, end: u64): vector<u8> {
    let mut result = vector[];
    let mut i = start;
    while (i < end) {
        vector::push_back(&mut result, *vector::borrow(v, i));
        i = i + 1;
    };
    result
}

/// Called by Seal key servers inside a PTB signed by the requester.
/// `id` = the Seal IBE identity bytes (package_id ++ repo_id, 64 bytes).
///
/// Security: verifies that `acl` is actually bound to the repository
/// identified by `id`, preventing cross-repo key extraction by users who
/// hold read access to a different repository in the same package.
entry fun seal_approve(id: vector<u8>, acl: &AccessControl, ctx: &TxContext) {
    assert!(vector::length(&id) == 64, ENoAccess);
    let repo_id_bytes = bytes_slice(&id, 32, 64);
    assert!(repo_id_bytes == object::id_to_bytes(&acl.repo_id), ENoAccess);

    let sender = ctx.sender();
    assert!(
        sender == acl.owner ||
        vector::contains(&acl.allowed_readers, &sender),
        ENoAccess,
    );
}

// ─── Repository lifecycle ─────────────────────────────────────────────────────

/// Create a new repository. Both Repository and AccessControl are shared
/// so any authorized collaborator can push commits or reference them in PTBs.
/// The repo UID is pre-allocated so the ACL can store the repo's ID,
/// creating a bidirectional binding verified on every write.
public fun create_repository(
    name: String,
    description: String,
    is_private: bool,
    clock: &Clock,
    ctx: &mut TxContext,
) {
    let sender = ctx.sender();

    let repo_uid = object::new(ctx);
    let repo_id = object::uid_to_inner(&repo_uid);

    let acl = AccessControl {
        id: object::new(ctx),
        owner: sender,
        repo_id,
        allowed_readers: vector[],
        allowed_writers: vector[],
    };
    let acl_id = object::id(&acl);

    let now = clock.timestamp_ms();
    let repo = Repository {
        id: repo_uid,
        owner: sender,
        name,
        description,
        is_private,
        branches: table::new(ctx),
        created_at: now,
        acl_id,
        forked_by: vec_set::empty(),
        next_pr_number: 1,
    };

    event::emit(RepositoryCreated {
        repo_id,
        acl_id,
        owner: sender,
        name: repo.name,
        description: repo.description,
        is_private: repo.is_private,
        created_at: now,
    });

    transfer::share_object(acl);
    transfer::share_object(repo);
}

/// Push a new commit to a branch.
/// `parent_id` is the Sui object ID of the previous Commit (32 raw bytes),
/// or an empty vector for the first push.
/// Caller must be the repository owner or a granted writer.
///
/// Security: verifies that `acl` is the ACL bound to `repo`.
public fun push_commit(
    repo: &mut Repository,
    acl: &AccessControl,
    blob_id: String,
    git_head: String,
    parent_id: vector<u8>,
    message: String,
    branch_name: String,
    clock: &Clock,
    ctx: &mut TxContext,
) {
    assert!(object::id(acl) == repo.acl_id, EAclMismatch);

    let sender = ctx.sender();
    assert!(
        sender == acl.owner ||
        vector::contains(&acl.allowed_writers, &sender),
        ENotOwner,
    );

    let parent: Option<ID> = if (parent_id.is_empty()) {
        option::none()
    } else {
        option::some(object::id_from_bytes(parent_id))
    };

    let now = clock.timestamp_ms();
    let commit = Commit {
        id: object::new(ctx),
        repo_id: object::id(repo),
        blob_id,
        git_head,
        parent,
        message,
        author: sender,
        timestamp: now,
    };

    let commit_id = object::id(&commit);

    if (table::contains(&repo.branches, branch_name)) {
        *table::borrow_mut(&mut repo.branches, branch_name) = commit_id;
    } else {
        table::add(&mut repo.branches, branch_name, commit_id);
    };

    event::emit(CommitPushed {
        repo_id: object::id(repo),
        commit_id,
        branch_name,
        git_head: commit.git_head,
        blob_id: commit.blob_id,
        author: sender,
        timestamp: now,
    });

    transfer::share_object(commit);
}

/// Fork a public repository into the caller's account.
public fun fork_repository(
    original_repo: &mut Repository,
    name: String,
    description: String,
    clock: &Clock,
    ctx: &mut TxContext,
) {
    assert!(!original_repo.is_private, ENoAccess);

    let sender = ctx.sender();
    assert!(sender != original_repo.owner, ESelfFork);
    assert!(!vec_set::contains(&original_repo.forked_by, &sender), EAlreadyForked);

    let original_repo_id = object::id(original_repo);

    let repo_uid = object::new(ctx);
    let fork_repo_id = object::uid_to_inner(&repo_uid);

    let acl = AccessControl {
        id: object::new(ctx),
        owner: sender,
        repo_id: fork_repo_id,
        allowed_readers: vector[],
        allowed_writers: vector[],
    };
    let acl_id = object::id(&acl);

    let now = clock.timestamp_ms();
    let fork = Repository {
        id: repo_uid,
        owner: sender,
        name,
        description,
        is_private: false,
        branches: table::new(ctx),
        created_at: now,
        acl_id,
        forked_by: vec_set::empty(),
        next_pr_number: 1,
    };

    event::emit(RepositoryForked {
        original_repo_id,
        fork_repo_id,
        fork_acl_id: acl_id,
        forked_by: sender,
        fork_name: fork.name,
        forked_at: now,
    });

    vec_set::insert(&mut original_repo.forked_by, sender);

    transfer::share_object(acl);
    transfer::share_object(fork);
}

/// Delete a repository. Only the owner can delete.
/// Branches Table must be empty.
public fun delete_repository(repo: Repository, ctx: &TxContext) {
    assert!(ctx.sender() == repo.owner, ENotOwner);

    let repo_id = object::id(&repo);
    let owner = repo.owner;

    let Repository {
        id,
        owner: _,
        name: _,
        description: _,
        is_private: _,
        branches,
        created_at: _,
        acl_id: _,
        forked_by: _,
        next_pr_number: _,
    } = repo;
    table::destroy_empty(branches);
    object::delete(id);

    event::emit(RepositoryDeleted { repo_id, owner });
}

// ─── Access control management ────────────────────────────────────────────────

public fun grant_read_access(acl: &mut AccessControl, reader: address, ctx: &TxContext) {
    assert!(ctx.sender() == acl.owner, ENotOwner);
    if (!vector::contains(&acl.allowed_readers, &reader)) {
        vector::push_back(&mut acl.allowed_readers, reader);
        event::emit(AccessChanged {
            acl_id: object::id(acl),
            repo_id: acl.repo_id,
            address: reader,
            role: 0,
            granted: true,
        });
    };
}

public fun revoke_read_access(acl: &mut AccessControl, reader: address, ctx: &TxContext) {
    assert!(ctx.sender() == acl.owner, ENotOwner);
    let (found, idx) = vector::index_of(&acl.allowed_readers, &reader);
    if (found) {
        vector::remove(&mut acl.allowed_readers, idx);
        event::emit(AccessChanged {
            acl_id: object::id(acl),
            repo_id: acl.repo_id,
            address: reader,
            role: 0,
            granted: false,
        });
    };
}

public fun grant_write_access(acl: &mut AccessControl, writer: address, ctx: &TxContext) {
    assert!(ctx.sender() == acl.owner, ENotOwner);
    if (!vector::contains(&acl.allowed_writers, &writer)) {
        vector::push_back(&mut acl.allowed_writers, writer);
        event::emit(AccessChanged {
            acl_id: object::id(acl),
            repo_id: acl.repo_id,
            address: writer,
            role: 1,
            granted: true,
        });
    };
}

public fun revoke_write_access(acl: &mut AccessControl, writer: address, ctx: &TxContext) {
    assert!(ctx.sender() == acl.owner, ENotOwner);
    let (found, idx) = vector::index_of(&acl.allowed_writers, &writer);
    if (found) {
        vector::remove(&mut acl.allowed_writers, idx);
        event::emit(AccessChanged {
            acl_id: object::id(acl),
            repo_id: acl.repo_id,
            address: writer,
            role: 1,
            granted: false,
        });
    };
}

// ─── Internal helpers used by sibling modules ─────────────────────────────────

/// Allocate and return the next PR number for this repository.
/// Called by `pull_request::create_pull_request` so the counter lives
/// in a single place and stays monotonic across concurrent creates.
public(package) fun next_pr_number(repo: &mut Repository): u64 {
    let n = repo.next_pr_number;
    repo.next_pr_number = n + 1;
    n
}

// ─── View functions ────────────────────────────────────────────────────────────

public fun get_branch_head(repo: &Repository, branch_name: String): Option<ID> {
    if (table::contains(&repo.branches, branch_name)) {
        option::some(*table::borrow(&repo.branches, branch_name))
    } else {
        option::none()
    }
}

public fun get_repo_name(repo: &Repository): String { repo.name }

public fun get_repo_owner(repo: &Repository): address { repo.owner }

public fun is_private(repo: &Repository): bool { repo.is_private }

public fun get_acl_id(repo: &Repository): ID { repo.acl_id }

public fun get_acl_owner(acl: &AccessControl): address { acl.owner }

public fun get_acl_repo_id(acl: &AccessControl): ID { acl.repo_id }

public fun get_allowed_readers(acl: &AccessControl): &vector<address> { &acl.allowed_readers }

public fun get_allowed_writers(acl: &AccessControl): &vector<address> { &acl.allowed_writers }

public fun get_commit_blob_id(commit: &Commit): String { commit.blob_id }

public fun get_commit_git_head(commit: &Commit): String { commit.git_head }

public fun get_commit_parent(commit: &Commit): Option<ID> { commit.parent }

public fun get_commit_message(commit: &Commit): String { commit.message }

public fun get_commit_author(commit: &Commit): address { commit.author }

public fun get_commit_timestamp(commit: &Commit): u64 { commit.timestamp }
