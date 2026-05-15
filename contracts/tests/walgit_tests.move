// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

#[test_only]
module walgit::walgit_tests;

use std::string;
use sui::clock;
use sui::test_scenario::{Self as ts, Scenario};
use walgit::walgit;

const OWNER: address = @0xA;
const OTHER: address = @0xB;
const FORKER: address = @0xC;
const FORKER2: address = @0xD;

/// Publish the package (creates the shared Registry) and then create
/// the OWNER's main public repo.
fun setup(scenario: &mut Scenario) {
    ts::next_tx(scenario, OWNER);
    { walgit::init_for_testing(ts::ctx(scenario)); };

    ts::next_tx(scenario, OWNER);
    {
        let mut registry = ts::take_shared<walgit::Registry>(scenario);
        let clock = clock::create_for_testing(ts::ctx(scenario));
        walgit::create_repository(
            &mut registry,
            string::utf8(b"my-repo"),
            string::utf8(b"A test repository"),
            false,
            &clock,
            ts::ctx(scenario),
        );
        clock::destroy_for_testing(clock);
        ts::return_shared(registry);
    };
}

fun setup_private(scenario: &mut Scenario) {
    ts::next_tx(scenario, OWNER);
    { walgit::init_for_testing(ts::ctx(scenario)); };

    ts::next_tx(scenario, OWNER);
    {
        let mut registry = ts::take_shared<walgit::Registry>(scenario);
        let clock = clock::create_for_testing(ts::ctx(scenario));
        walgit::create_repository(
            &mut registry,
            string::utf8(b"private-repo"),
            string::utf8(b""),
            true,
            &clock,
            ts::ctx(scenario),
        );
        clock::destroy_for_testing(clock);
        ts::return_shared(registry);
    };
}

fun do_fork(scenario: &mut Scenario, forker: address) {
    ts::next_tx(scenario, forker);
    {
        let mut registry = ts::take_shared<walgit::Registry>(scenario);
        let mut repo = ts::take_shared<walgit::Repository>(scenario);
        let clock = clock::create_for_testing(ts::ctx(scenario));
        walgit::fork_repository(
            &mut registry,
            &mut repo,
            string::utf8(b"my-repo-fork"),
            string::utf8(b"a fork"),
            &clock,
            ts::ctx(scenario),
        );
        clock::destroy_for_testing(clock);
        ts::return_shared(repo);
        ts::return_shared(registry);
    };
}

#[test]
fun test_create_repository() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);

    ts::next_tx(&mut scenario, OWNER);
    {
        assert!(ts::has_most_recent_shared<walgit::Repository>(), 0);
        assert!(ts::has_most_recent_shared<walgit::AccessControl>(), 1);
    };

    ts::end(scenario);
}

#[test]
fun test_push_commit() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);

    ts::next_tx(&mut scenario, OWNER);
    {
        let mut repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));

        walgit::push_commit(
            &mut repo,
            &acl,
            string::utf8(b"abc123walrusblobid"),
            string::utf8(b"deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
            vector[],
            string::utf8(b"initial commit"),
            string::utf8(b"main"),
            &clock,
            ts::ctx(&mut scenario),
        );

        let head = walgit::get_branch_head(&repo, string::utf8(b"main"));
        assert!(option::is_some(&head), 2);

        clock::destroy_for_testing(clock);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::end(scenario);
}

#[test]
#[expected_failure(abort_code = walgit::ENotOwner)]
fun test_push_commit_not_owner_fails() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);

    ts::next_tx(&mut scenario, OTHER);
    {
        let mut repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));

        walgit::push_commit(
            &mut repo,
            &acl,
            string::utf8(b"blob"),
            string::utf8(b"githead"),
            vector[],
            string::utf8(b"hack"),
            string::utf8(b"main"),
            &clock,
            ts::ctx(&mut scenario),
        );

        clock::destroy_for_testing(clock);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::end(scenario);
}

#[test]
fun test_push_commit_writer_allowed() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);

    // OWNER grants OTHER write access.
    ts::next_tx(&mut scenario, OWNER);
    {
        let mut acl = ts::take_shared<walgit::AccessControl>(&scenario);
        walgit::grant_write_access(&mut acl, OTHER, ts::ctx(&mut scenario));
        ts::return_shared(acl);
    };

    // OTHER pushes a commit — should succeed.
    ts::next_tx(&mut scenario, OTHER);
    {
        let mut repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));

        walgit::push_commit(
            &mut repo,
            &acl,
            string::utf8(b"blob"),
            string::utf8(b"githead"),
            vector[],
            string::utf8(b"by-writer"),
            string::utf8(b"feat"),
            &clock,
            ts::ctx(&mut scenario),
        );

        let head = walgit::get_branch_head(&repo, string::utf8(b"feat"));
        assert!(option::is_some(&head), 0);

        clock::destroy_for_testing(clock);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::end(scenario);
}

// ─── Fork tests ───────────────────────────────────────────────────────────────

#[test]
fun test_fork_creates_shared_objects() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);
    do_fork(&mut scenario, FORKER);

    ts::next_tx(&mut scenario, FORKER);
    {
        assert!(ts::has_most_recent_shared<walgit::Repository>(), 0);
        assert!(ts::has_most_recent_shared<walgit::AccessControl>(), 1);
    };

    ts::end(scenario);
}

#[test]
fun test_fork_owner_is_forker() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);
    do_fork(&mut scenario, FORKER);

    ts::next_tx(&mut scenario, FORKER);
    {
        let original = ts::take_shared<walgit::Repository>(&scenario);
        let fork = ts::take_shared<walgit::Repository>(&scenario);
        assert!(walgit::get_repo_owner(&fork) == FORKER, 0);
        ts::return_shared(fork);
        ts::return_shared(original);
    };

    ts::end(scenario);
}

#[test]
fun test_fork_is_public() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);
    do_fork(&mut scenario, FORKER);

    ts::next_tx(&mut scenario, FORKER);
    {
        let fork = ts::take_shared<walgit::Repository>(&scenario);
        assert!(!walgit::is_private(&fork), 0);
        ts::return_shared(fork);
    };

    ts::end(scenario);
}

#[test]
fun test_two_different_forkers_allowed() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);
    do_fork(&mut scenario, FORKER);
    do_fork(&mut scenario, FORKER2);
    ts::end(scenario);
}

#[test]
#[expected_failure(abort_code = walgit::ESelfFork)]
fun test_fork_own_repo_fails() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);
    do_fork(&mut scenario, OWNER);
    ts::end(scenario);
}

#[test]
#[expected_failure(abort_code = walgit::EAlreadyForked)]
fun test_fork_duplicate_fails() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);
    do_fork(&mut scenario, FORKER);
    do_fork(&mut scenario, FORKER);
    ts::end(scenario);
}

#[test]
#[expected_failure(abort_code = walgit::ENoAccess)]
fun test_fork_private_repo_fails() {
    let mut scenario = ts::begin(OWNER);
    setup_private(&mut scenario);

    ts::next_tx(&mut scenario, FORKER);
    {
        let mut registry = ts::take_shared<walgit::Registry>(&scenario);
        let mut repo = ts::take_shared<walgit::Repository>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));
        walgit::fork_repository(
            &mut registry,
            &mut repo,
            string::utf8(b"fork"),
            string::utf8(b""),
            &clock,
            ts::ctx(&mut scenario),
        );
        clock::destroy_for_testing(clock);
        ts::return_shared(repo);
        ts::return_shared(registry);
    };

    ts::end(scenario);
}

// ─── Registry / uniqueness tests ──────────────────────────────────────────────

#[test]
#[expected_failure(abort_code = walgit::ENameTaken)]
fun test_duplicate_name_same_owner_fails() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);

    // Second create_repository with the SAME name from the SAME owner must abort.
    ts::next_tx(&mut scenario, OWNER);
    {
        let mut registry = ts::take_shared<walgit::Registry>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));
        walgit::create_repository(
            &mut registry,
            string::utf8(b"my-repo"),
            string::utf8(b""),
            false,
            &clock,
            ts::ctx(&mut scenario),
        );
        clock::destroy_for_testing(clock);
        ts::return_shared(registry);
    };
    ts::end(scenario);
}

#[test]
fun test_same_name_different_owners_ok() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);

    // OTHER creates a repo with the same name — should succeed because
    // uniqueness is scoped per (owner, name), not globally.
    ts::next_tx(&mut scenario, OTHER);
    {
        let mut registry = ts::take_shared<walgit::Registry>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));
        walgit::create_repository(
            &mut registry,
            string::utf8(b"my-repo"),
            string::utf8(b""),
            false,
            &clock,
            ts::ctx(&mut scenario),
        );
        clock::destroy_for_testing(clock);
        ts::return_shared(registry);
    };
    ts::end(scenario);
}

#[test]
#[expected_failure(abort_code = walgit::ENameTaken)]
fun test_fork_with_taken_name_fails() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);

    // FORKER first creates their own repo named "my-repo-fork".
    ts::next_tx(&mut scenario, FORKER);
    {
        let mut registry = ts::take_shared<walgit::Registry>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));
        walgit::create_repository(
            &mut registry,
            string::utf8(b"my-repo-fork"),
            string::utf8(b""),
            false,
            &clock,
            ts::ctx(&mut scenario),
        );
        clock::destroy_for_testing(clock);
        ts::return_shared(registry);
    };

    // FORKER tries to fork OWNER's repo into the SAME name → ENameTaken.
    // Must explicitly take both Repository objects so we fork the right one
    // (LIFO take_shared otherwise hands back FORKER's own repo).
    ts::next_tx(&mut scenario, FORKER);
    {
        let mut registry = ts::take_shared<walgit::Registry>(&scenario);
        let forkers_own = ts::take_shared<walgit::Repository>(&scenario);
        let mut owners_repo = ts::take_shared<walgit::Repository>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));
        walgit::fork_repository(
            &mut registry,
            &mut owners_repo,
            string::utf8(b"my-repo-fork"),
            string::utf8(b""),
            &clock,
            ts::ctx(&mut scenario),
        );
        clock::destroy_for_testing(clock);
        ts::return_shared(owners_repo);
        ts::return_shared(forkers_own);
        ts::return_shared(registry);
    };
    ts::end(scenario);
}

// ─── Access tests ─────────────────────────────────────────────────────────────

#[test]
#[expected_failure(abort_code = walgit::ENotOwner)]
fun test_grant_access_not_owner_fails() {
    let mut scenario = ts::begin(OWNER);
    setup(&mut scenario);

    ts::next_tx(&mut scenario, OTHER);
    {
        let mut acl = ts::take_shared<walgit::AccessControl>(&scenario);
        walgit::grant_read_access(&mut acl, OTHER, ts::ctx(&mut scenario));
        ts::return_shared(acl);
    };

    ts::end(scenario);
}
