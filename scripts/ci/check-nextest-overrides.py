#!/usr/bin/env python3
"""Require each configured override to match exactly its reviewed runnable identities.

Filters belong exclusively to nextest.toml: nextest evaluates their original text.
Registration is deliberately explicit; never learn expected identities from a run.
Archive listing uses nextest's temporary extraction, never the shared ./target.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]
# Keys identify the reviewed override slots; every profile/slot must be registered.
# Identity = (package-name, binary-id, test-name), independent of build paths/hashes.
REVIEWED = {
    ("ci", 0): frozenset([
        ('calm-proc-supervisor', 'calm-proc-supervisor', 'pgid_lease_tests::group_signal_error_replies_are_distinguishable_by_kind_and_prefix'),
        ('calm-proc-supervisor', 'calm-proc-supervisor', 'pgid_lease_tests::pipe_target_is_ok_kind_readable_and_refused_by_the_signal_rpc'),
        ('calm-proc-supervisor', 'calm-proc-supervisor', 'wnowait_tests::group_target_refuses_the_leader_target_after_pin_loss'),
        ('calm-proc-supervisor', 'calm-proc-supervisor', 'wnowait_tests::proc_exit_parts_from_siginfo_maps_every_wexited_code'),
        ('calm-proc-supervisor', 'calm-proc-supervisor', 'wnowait_tests::seal_and_publish_exit_stamps_and_broadcasts_exactly_once'),
        ('calm-proc-supervisor', 'calm-proc-supervisor', 'wnowait_tests::waiter_completion_guard_makes_a_panicked_waiter_reclaimable'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'attach_race_no_byte_loss::attach_race_no_byte_loss'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'every_root_test_file_is_in_this_suite'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'exit_status_survives_the_wnowait_split::exit_status_survives_the_wnowait_split'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'explicit_child_environment::pty_children_exclude_application_credentials_but_keep_explicit_environment'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'no_wildcard_wait_in_the_supervisor_host::no_wildcard_wait_in_the_supervisor_host'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pipe_procs_are_not_signalable::pipe_procs_are_not_signalable_via_the_signal_rpc'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_cwd::pty_cwd_empty_preserves_home_default'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_cwd::pty_cwd_missing_directory_is_rejected_without_execution_or_registration'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_cwd::pty_cwd_regular_file_is_rejected_without_execution_or_registration'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_cwd::pty_cwd_relative_directory_resolves_against_supervisor_directory'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_cwd::pty_cwd_valid_directory_runs_in_requested_directory'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_cwd::pty_cwd_validation_keeps_pipe_bootstrap_missing_directory_contract'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_entry_reclaim::cleanup_removes_the_entry_without_waiting_for_the_grace'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_entry_reclaim::entry_removal_reaps_even_when_the_grandchild_holds_the_pty'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_entry_reclaim::expired_pty_entries_are_removed_and_release_ring_and_fds'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_entry_reclaim::no_eof_is_injected_when_the_pty_reader_dies_without_eof'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_entry_reclaim::replay_and_sticky_exit_survive_within_grace'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_exit_ordering::exit_cursor_equals_final_byte_count'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_exit_ordering::exited_is_final_when_a_grandchild_holds_the_pty_open'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_exit_ordering::exited_is_the_last_frame_for_a_live_attacher'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_exit_ordering::the_reader_keeps_draining_the_master_after_the_seal'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_pid_pinning::orphaned_entries_are_visible_to_the_pin_counter'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_pid_pinning::pty_pid_stays_pinned_while_the_entry_is_registered'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_pid_pinning::the_pinned_pid_is_released_when_the_entry_is_removed'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_proc_byte_stream_and_replay::pty_proc_byte_stream_and_replay'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'pty_writestdin_acked::pty_writestdin_acked'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'server_restart_survives::proc_outlives_client_disconnect_and_dies_with_supervisor'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'signal_terminates_pty_child::signal_targets_spawned_leader_not_tty_foreground_job'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'signal_terminates_pty_child::signal_terminates_pty_child'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'terminate_all_after_exit_recorded::terminate_all_kills_grandchild_after_the_exit_is_recorded'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'terminate_all_kills_grandchild::terminate_all_kills_grandchild_of_live_leader'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::integration_suite', 'terminate_all_kills_grandchild_in_drain_grace::terminate_all_kills_grandchild_inside_drain_grace'),
        ('calm-proc-supervisor', 'calm-proc-supervisor::pin_lost_on_autoreap', 'pin_lost_when_the_child_is_autoreaped'),
        ('calm-server', 'calm-server::api_suite', 'domain_api_suite::in_process_renderer_e2e::drop_entry_keeps_the_term_grace_when_the_attach_reader_died_early'),
        ('calm-server', 'calm-server::api_suite', 'domain_api_suite::in_process_renderer_e2e::drop_entry_kills_process_group_members_that_outlive_the_leader'),
        ('calm-server', 'calm-server::api_suite', 'domain_api_suite::in_process_renderer_e2e::drop_entry_persists_the_terminal_exit_to_the_database'),
        ('calm-server', 'calm-server::api_suite', 'domain_api_suite::in_process_renderer_e2e::in_process_renderer_drives_real_supervisor_and_pty'),
        ('calm-server', 'calm-server::api_suite', 'domain_api_suite::in_process_renderer_e2e::late_client_attach_receives_sticky_terminal_exited'),
        ('calm-server', 'calm-server::api_suite', 'domain_api_suite::in_process_renderer_e2e::registry_ensure_lazily_reattaches_when_registry_is_empty'),
        ('calm-server', 'calm-server::runtime_suite', 'kernel_process_suite::reconcile_supervisor_on_boot::live_ephemeral_terminal_exit_still_completes_its_session'),
        ('calm-server', 'calm-server::runtime_suite', 'kernel_process_suite::reconcile_supervisor_on_boot::live_terminal_left_alone'),
        ('calm-server', 'calm-server::runtime_suite', 'kernel_process_suite::reconcile_supervisor_on_boot::missing_ephemeral_terminal_still_completes_its_session'),
        ('calm-server', 'calm-server::runtime_suite', 'kernel_process_suite::reconcile_supervisor_on_boot::missing_viewer_preserves_resumable_session_but_explicit_completion_still_works'),
        ('calm-server', 'calm-server::runtime_suite', 'kernel_process_suite::reconcile_supervisor_on_boot::probe_error_leaves_row_unchanged'),
        ('calm-server', 'calm-server::runtime_suite', 'kernel_process_suite::reconcile_supervisor_on_boot::stale_terminal_row_marked_exited'),

    ]),
    ("ci", 1): frozenset([
        ("calm-server", "calm-server::runtime_suite",
         "migration_suite::migration_replay_harness::synthetic_fixture_replays_from_every_supported_version"),
    ]),
}


def runnable(output):
    data = json.loads(output)
    if not isinstance(data, dict):
        raise ValueError("nextest output must be an object")
    suites = data["rust-suites"]
    if not isinstance(suites, dict):
        raise ValueError("rust-suites must be an object")
    matches = set()
    for key, suite in suites.items():
        if not isinstance(suite, dict) or not isinstance(suite.get("testcases"), dict):
            raise ValueError("invalid suite/testcases object")
        package, binary = suite["package-name"], suite["binary-id"]
        if not all(isinstance(x, str) and x for x in (package, binary)) or key != binary:
            raise ValueError("invalid package/binary identity")
        if suite["status"] != "listed":
            raise ValueError(f"unlisted binary: {binary}")
        for name, test in suite["testcases"].items():
            if not isinstance(test, dict) or not isinstance(test.get("filter-match"), dict):
                raise ValueError("invalid testcase/filter-match object")
            status = test["filter-match"]["status"]
            if status not in ("matches", "mismatch") or type(test["ignored"]) is not bool:
                raise ValueError(f"invalid test status: {binary}::{name}")
            if not isinstance(name, str) or not name:
                raise ValueError("invalid test identity")
            if status == "matches" and not test["ignored"]:
                matches.add((package, binary, name))
    return matches


def check(config, source_args, query=None):
    slots = {(profile, index): override
             for profile, settings in config["profile"].items()
             for index, override in enumerate(settings.get("overrides", []))}
    if slots.keys() != REVIEWED.keys():
        raise ValueError(f"unregistered overrides: {sorted(slots.keys() - REVIEWED.keys())}; "
                         f"missing registrations: {sorted(REVIEWED.keys() - slots.keys())}")
    env = dict(os.environ)
    env.pop("NEIGE_CODEX_BIN", None)
    failed = False
    for slot, override in slots.items():
        expression = override["filter"]
        if not isinstance(expression, str) or not expression:
            raise ValueError(f"invalid filter: {slot}")
        command = ["cargo", "nextest", "list", *source_args, "--profile", slot[0],
                   "--message-format", "json", "-E", expression]
        output = (query(command) if query else subprocess.run(
            command, cwd=ROOT, env=env, check=True, stdout=subprocess.PIPE, text=True).stdout)
        actual = runnable(output)
        expected = REVIEWED[slot]
        missing, unexpected = expected - actual, actual - expected
        if not actual or missing or unexpected:
            failed = True
            print(f"override {slot}: runnable={len(actual)}; empty={not actual}", file=sys.stderr)
            for label, difference in (("missing", missing), ("unexpected", unexpected)):
                print(f"{label}: {json.dumps(sorted(difference))}", file=sys.stderr)
        else:
            print(f"override {slot}: {len(actual)} reviewed runnable tests")
    return not failed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive-file")
    args = parser.parse_args()
    source = (["--archive-file", args.archive_file, "--workspace-remap", str(ROOT)]
              if args.archive_file else
              ["--workspace", "--locked", "--features", "calm-server/codex-e2e"])
    try:
        with (ROOT / ".config/nextest.toml").open("rb") as stream:
            config = tomllib.load(stream)
        return 0 if check(config, source) else 1
    except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
        print(f"nextest override guard failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
