// Copyright (c) 2026 Nikita Vatletsov
// SPDX-License-Identifier: Apache-2.0

#[test_only]
module walgit::pull_request_tests;

use std::string;
use sui::clock;
use sui::test_scenario::{Self as ts, Scenario};
use walgit::walgit;
use walgit::pull_request;

const OWNER: address = @0xA;
const WRITER: address = @0xB;
const READER: address = @0xC;
const STRANGER: address = @0xD;
const PR_AUTHOR: address = @0xE;

// ─── Setup helpers ────────────────────────────────────────────────────────────

fun setup_public(scenario: &mut Scenario) {
    ts::next_tx(scenario, OWNER);
    {
        let clock = clock::create_for_testing(ts::ctx(scenario));
        walgit::create_repository(
            string::utf8(b"test-repo"),
            string::utf8(b""),
            false,
            &clock,
            ts::ctx(scenario),
        );
        clock::destroy_for_testing(clock);
    };
}

fun setup_private(scenario: &mut Scenario) {
    ts::next_tx(scenario, OWNER);
    {
        let clock = clock::create_for_testing(ts::ctx(scenario));
        walgit::create_repository(
            string::utf8(b"private-repo"),
            string::utf8(b""),
            true,
            &clock,
            ts::ctx(scenario),
        );
        clock::destroy_for_testing(clock);
    };
}

fun grant_writer(scenario: &mut Scenario) {
    ts::next_tx(scenario, OWNER);
    {
        let mut acl = ts::take_shared<walgit::AccessControl>(scenario);
        walgit::grant_write_access(&mut acl, WRITER, ts::ctx(scenario));
        ts::return_shared(acl);
    };
}

fun grant_reader(scenario: &mut Scenario) {
    ts::next_tx(scenario, OWNER);
    {
        let mut acl = ts::take_shared<walgit::AccessControl>(scenario);
        walgit::grant_read_access(&mut acl, READER, ts::ctx(scenario));
        ts::return_shared(acl);
    };
}

fun open_pr(scenario: &mut Scenario, author: address) {
    ts::next_tx(scenario, author);
    {
        let mut repo = ts::take_shared<walgit::Repository>(scenario);
        let acl = ts::take_shared<walgit::AccessControl>(scenario);
        let clock = clock::create_for_testing(ts::ctx(scenario));
        pull_request::create_pull_request(
            &mut repo,
            &acl,
            string::utf8(b"feature"),
            string::utf8(b"main"),
            string::utf8(b"sourceblobid"),
            &clock,
            ts::ctx(scenario),
        );
        clock::destroy_for_testing(clock);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };
}

// ─── create_pull_request ─────────────────────────────────────────────────────

#[test]
fun test_create_pr_public_repo_anyone() {
    let mut scenario = ts::begin(OWNER);
    setup_public(&mut scenario);
    open_pr(&mut scenario, STRANGER);

    ts::next_tx(&mut scenario, STRANGER);
    {
        assert!(ts::has_most_recent_shared<pull_request::PullRequest>(), 0);
        let pr = ts::take_shared<pull_request::PullRequest>(&scenario);
        assert!(pull_request::get_pr_number(&pr) == 1, 1);
        assert!(pull_request::get_pr_author(&pr) == STRANGER, 2);
        assert!(pull_request::get_pr_approved(&pr) == false, 3);
        assert!(pull_request::get_pr_status(&pr) == 0, 4);
        ts::return_shared(pr);
    };

    ts::end(scenario);
}

#[test]
fun test_pr_numbers_are_monotonic() {
    let mut scenario = ts::begin(OWNER);
    setup_public(&mut scenario);
    open_pr(&mut scenario, STRANGER);
    open_pr(&mut scenario, PR_AUTHOR);

    ts::next_tx(&mut scenario, OWNER);
    {
        // Two PRs created — numbers should be 1 and 2.
        let pr_a = ts::take_shared<pull_request::PullRequest>(&scenario);
        let pr_b = ts::take_shared<pull_request::PullRequest>(&scenario);
        let n1 = pull_request::get_pr_number(&pr_a);
        let n2 = pull_request::get_pr_number(&pr_b);
        // Allocation order isn't fixed by take_shared LIFO, so just assert {1,2}.
        assert!((n1 == 1 && n2 == 2) || (n1 == 2 && n2 == 1), 0);
        ts::return_shared(pr_a);
        ts::return_shared(pr_b);
    };

    ts::end(scenario);
}

#[test]
fun test_create_pr_private_repo_owner() {
    let mut scenario = ts::begin(OWNER);
    setup_private(&mut scenario);
    open_pr(&mut scenario, OWNER);

    ts::next_tx(&mut scenario, OWNER);
    {
        assert!(ts::has_most_recent_shared<pull_request::PullRequest>(), 0);
    };

    ts::end(scenario);
}

#[test]
fun test_create_pr_private_repo_writer() {
    let mut scenario = ts::begin(OWNER);
    setup_private(&mut scenario);
    grant_writer(&mut scenario);
    open_pr(&mut scenario, WRITER);
    ts::end(scenario);
}

#[test]
fun test_create_pr_private_repo_reader() {
    let mut scenario = ts::begin(OWNER);
    setup_private(&mut scenario);
    grant_reader(&mut scenario);
    open_pr(&mut scenario, READER);
    ts::end(scenario);
}

#[test]
#[expected_failure(abort_code = pull_request::ENoAccess)]
fun test_create_pr_private_repo_stranger_fails() {
    let mut scenario = ts::begin(OWNER);
    setup_private(&mut scenario);
    open_pr(&mut scenario, STRANGER);
    ts::end(scenario);
}

// ─── approve_pull_request ─────────────────────────────────────────────────────

#[test]
fun test_approve_pr_by_writer() {
    let mut scenario = ts::begin(OWNER);
    setup_public(&mut scenario);
    grant_writer(&mut scenario);
    open_pr(&mut scenario, PR_AUTHOR);

    ts::next_tx(&mut scenario, WRITER);
    {
        let mut pr = ts::take_shared<pull_request::PullRequest>(&scenario);
        let repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));

        pull_request::approve_pull_request(&mut pr, &repo, &acl, &clock, ts::ctx(&mut scenario));

        assert!(pull_request::get_pr_approved(&pr) == true, 0);
        let approved_by = pull_request::get_pr_approved_by(&pr);
        assert!(option::is_some(&approved_by), 1);
        assert!(*option::borrow(&approved_by) == WRITER, 2);

        clock::destroy_for_testing(clock);
        ts::return_shared(pr);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::end(scenario);
}

#[test]
#[expected_failure(abort_code = pull_request::ESelfApprove)]
fun test_approve_pr_self_fails() {
    let mut scenario = ts::begin(OWNER);
    setup_public(&mut scenario);
    open_pr(&mut scenario, OWNER);

    ts::next_tx(&mut scenario, OWNER);
    {
        let mut pr = ts::take_shared<pull_request::PullRequest>(&scenario);
        let repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));
        pull_request::approve_pull_request(&mut pr, &repo, &acl, &clock, ts::ctx(&mut scenario));
        clock::destroy_for_testing(clock);
        ts::return_shared(pr);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::end(scenario);
}

#[test]
#[expected_failure(abort_code = pull_request::ENotWriter)]
fun test_approve_pr_not_writer_fails() {
    let mut scenario = ts::begin(OWNER);
    setup_public(&mut scenario);
    open_pr(&mut scenario, PR_AUTHOR);

    ts::next_tx(&mut scenario, STRANGER);
    {
        let mut pr = ts::take_shared<pull_request::PullRequest>(&scenario);
        let repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));
        pull_request::approve_pull_request(&mut pr, &repo, &acl, &clock, ts::ctx(&mut scenario));
        clock::destroy_for_testing(clock);
        ts::return_shared(pr);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::end(scenario);
}

// ─── merge_pull_request ───────────────────────────────────────────────────────

#[test]
fun test_merge_pr_after_approve() {
    let mut scenario = ts::begin(OWNER);
    setup_public(&mut scenario);
    grant_writer(&mut scenario);
    open_pr(&mut scenario, PR_AUTHOR);

    ts::next_tx(&mut scenario, WRITER);
    {
        let mut pr = ts::take_shared<pull_request::PullRequest>(&scenario);
        let repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));
        pull_request::approve_pull_request(&mut pr, &repo, &acl, &clock, ts::ctx(&mut scenario));
        clock::destroy_for_testing(clock);
        ts::return_shared(pr);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::next_tx(&mut scenario, OWNER);
    {
        let mut pr = ts::take_shared<pull_request::PullRequest>(&scenario);
        let repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));

        pull_request::merge_pull_request(
            &mut pr,
            &repo,
            &acl,
            string::utf8(b"mergeblobid"),
            &clock,
            ts::ctx(&mut scenario),
        );

        assert!(pull_request::get_pr_status(&pr) == 1, 0);
        let blob = pull_request::get_pr_merge_blob_id(&pr);
        assert!(option::is_some(&blob), 1);

        clock::destroy_for_testing(clock);
        ts::return_shared(pr);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::end(scenario);
}

#[test]
#[expected_failure(abort_code = pull_request::ENotApproved)]
fun test_merge_pr_not_approved_fails() {
    let mut scenario = ts::begin(OWNER);
    setup_public(&mut scenario);
    open_pr(&mut scenario, PR_AUTHOR);

    ts::next_tx(&mut scenario, OWNER);
    {
        let mut pr = ts::take_shared<pull_request::PullRequest>(&scenario);
        let repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));
        pull_request::merge_pull_request(
            &mut pr, &repo, &acl, string::utf8(b"blob"), &clock, ts::ctx(&mut scenario),
        );
        clock::destroy_for_testing(clock);
        ts::return_shared(pr);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::end(scenario);
}

// ─── close_pull_request ───────────────────────────────────────────────────────

#[test]
fun test_close_pr_by_owner() {
    let mut scenario = ts::begin(OWNER);
    setup_public(&mut scenario);
    open_pr(&mut scenario, PR_AUTHOR);

    ts::next_tx(&mut scenario, OWNER);
    {
        let mut pr = ts::take_shared<pull_request::PullRequest>(&scenario);
        let repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));

        pull_request::close_pull_request(&mut pr, &repo, &acl, &clock, ts::ctx(&mut scenario));
        assert!(pull_request::get_pr_status(&pr) == 2, 0);

        clock::destroy_for_testing(clock);
        ts::return_shared(pr);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::end(scenario);
}

#[test]
fun test_close_pr_by_author() {
    let mut scenario = ts::begin(OWNER);
    setup_public(&mut scenario);
    open_pr(&mut scenario, PR_AUTHOR);

    ts::next_tx(&mut scenario, PR_AUTHOR);
    {
        let mut pr = ts::take_shared<pull_request::PullRequest>(&scenario);
        let repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));

        pull_request::close_pull_request(&mut pr, &repo, &acl, &clock, ts::ctx(&mut scenario));
        assert!(pull_request::get_pr_status(&pr) == 2, 0);

        clock::destroy_for_testing(clock);
        ts::return_shared(pr);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::end(scenario);
}

#[test]
#[expected_failure(abort_code = pull_request::ENotOwner)]
fun test_close_pr_stranger_fails() {
    let mut scenario = ts::begin(OWNER);
    setup_public(&mut scenario);
    open_pr(&mut scenario, PR_AUTHOR);

    ts::next_tx(&mut scenario, STRANGER);
    {
        let mut pr = ts::take_shared<pull_request::PullRequest>(&scenario);
        let repo = ts::take_shared<walgit::Repository>(&scenario);
        let acl = ts::take_shared<walgit::AccessControl>(&scenario);
        let clock = clock::create_for_testing(ts::ctx(&mut scenario));
        pull_request::close_pull_request(&mut pr, &repo, &acl, &clock, ts::ctx(&mut scenario));
        clock::destroy_for_testing(clock);
        ts::return_shared(pr);
        ts::return_shared(repo);
        ts::return_shared(acl);
    };

    ts::end(scenario);
}
