// Shared helper tests belong to one suite so each test is registered once.

mod oracle {
    use crate::support::event_queries::EventRow;
    use crate::support::oracle::*;
    use serde_json::json;

    fn row(id: i64, payload: serde_json::Value) -> EventRow {
        EventRow {
            id,
            scope_kind: "track".to_string(),
            scope_track: Some("w1".to_string()),
            scope_card: None,
            payload,
        }
    }

    #[test]
    fn first_matching_id_picks_min() {
        let rows = vec![
            row(5, json!({"k":1})),
            row(2, json!({"k":1})),
            row(9, json!({"k":2})),
        ];
        assert_eq!(first_matching_id(&rows, |r| r.payload["k"] == 1), Some(2));
        assert_eq!(first_matching_id(&rows, |r| r.payload["k"] == 3), None);
    }

    #[test]
    fn skeleton_superset_missing_reports_absent_kind() {
        let required = [
            RequiredEvent::new("forge.pr.checks", |r| r.payload["conclusion"] == "success"),
            RequiredEvent::any("forge.pr.merged"),
        ];
        let checked = vec![
            (&required[0], vec![row(1, json!({"conclusion": "success"}))]),
            (&required[1], Vec::new()),
        ];
        assert_eq!(skeleton_superset_missing(&checked), vec!["forge.pr.merged"]);
    }

    #[test]
    fn ordering_violation_reports_only_misordered_present_edges() {
        let is_before = |r: &EventRow| r.payload["role"] == "before";
        let is_after = |r: &EventRow| r.payload["role"] == "after";

        assert_eq!(
            ordering_violation(
                &[row(1, json!({"role": "before"}))],
                &is_before,
                &[row(2, json!({"role": "after"}))],
                &is_after,
            ),
            None
        );
        assert_eq!(
            ordering_violation(
                &[row(5, json!({"role": "before"}))],
                &is_before,
                &[row(3, json!({"role": "after"}))],
                &is_after,
            ),
            Some((5, 3))
        );
        assert_eq!(
            ordering_violation(
                &[row(1, json!({"role": "before"}))],
                &is_before,
                &[row(2, json!({"role": "other"}))],
                &is_after,
            ),
            None
        );
        assert_eq!(
            ordering_violation(
                &[row(1, json!({"role": "other"}))],
                &is_before,
                &[row(2, json!({"role": "after"}))],
                &is_after,
            ),
            None
        );
    }
}

#[cfg(feature = "codex-e2e")]
mod agent_diag {
    use crate::support::agent_diag::*;

    #[test]
    fn summarize_rollout_jsonl_surfaces_tool_calls_args_and_outputs() {
        let transcript = concat!(
            r#"{"timestamp":"T0","type":"session_meta","payload":{"id":"sess-1","cwd":"/w","originator":"neige"}}"#,
            "\n",
            r#"{"timestamp":"T1","type":"response_item","payload":{"type":"function_call","name":"plugin_gitforge_git_commit","arguments":"{}","call_id":"call_1"}}"#,
            "\n",
            r#"{"timestamp":"T2","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"missing required message"}}"#,
            "\n",
            r#"{"timestamp":"T3","type":"event_msg","payload":{"type":"agent_message","message":"committing now"}}"#,
            "\n",
            r#"{"timestamp":"T4","type":"event_msg","payload":{"type":"turn_aborted","turn_id":"t1","reason":"interrupted"}}"#,
            "\n",
            "not json at all\n",
            r#"{"timestamp":"T5","type":"response_item","payload":{"type":"reasoning","encrypted_content":"xxx"}}"#,
            "\n",
        );
        let summary = summarize_rollout_jsonl(transcript);
        assert!(summary.contains("session_meta id=\"sess-1\""), "{summary}");
        assert!(
            summary.contains(
                "function_call \"plugin_gitforge_git_commit\" call_id=\"call_1\" arguments={}"
            ),
            "{summary}"
        );
        assert!(
            summary.contains("-> output call_id=\"call_1\" \"missing required message\""),
            "{summary}"
        );
        assert!(
            summary.contains("agent_message committing now"),
            "{summary}"
        );
        assert!(summary.contains("turn_aborted"), "{summary}");
        assert!(
            summary.contains("<unparseable json> not json at all"),
            "{summary}"
        );
        assert!(!summary.contains("encrypted_content"), "{summary}");
    }

    #[test]
    fn summarize_rollout_jsonl_handles_empty_transcript() {
        assert_eq!(
            summarize_rollout_jsonl(""),
            "  <no tool calls or notable events parsed>\n"
        );
    }

    #[test]
    fn find_rollout_files_scans_both_codex_home_roots() {
        let root = tempfile::tempdir().expect("tempdir");
        let shared = root.path().join("codex-home/sessions/2026/07/04");
        let per_thread = root.path().join("codex-homes/thread-a/sessions/2026/07/04");
        std::fs::create_dir_all(&shared).expect("mkdir shared sessions");
        std::fs::create_dir_all(&per_thread).expect("mkdir per-thread sessions");
        std::fs::write(shared.join("rollout-2026-07-04T00-00-00-a.jsonl"), "{}").expect("write");
        std::fs::write(per_thread.join("rollout-2026-07-04T00-00-01-b.jsonl"), "{}")
            .expect("write");
        std::fs::write(shared.join("history.jsonl"), "{}").expect("write");
        let found = find_rollout_files(root.path());
        assert_eq!(found.len(), 2, "{found:?}");
    }

    #[test]
    fn evidence_tempdir_cleans_on_success_and_leaks_on_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        drop(EvidenceTempDir::new(dir));
        assert!(!path.exists());

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            let _evidence = EvidenceTempDir::new(dir);
            panic!("intentional test panic for leak-on-panic coverage");
        });
        assert!(handle.join().is_err());
        assert!(path.exists(), "evidence dir must survive panic");
        std::fs::remove_dir_all(&path).expect("cleanup leaked evidence dir");
    }
}
