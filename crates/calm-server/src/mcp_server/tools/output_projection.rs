//! Gate step display selection; authorization and loading belong to the track reader.
pub(super) fn step_text(text: &str, selected: &str) -> String {
    let mut current = None;
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        // The wrapper terminates the marker, but does not insert a newline
        // before it. Keep any preceding output with the previous step, also
        // for already-recorded logs. Markers are display hints, never verdicts.
        let rest = if let Some((prefix, step)) = line.split_once("::gate-step ") {
            if current == Some(selected) {
                out.push_str(prefix);
            }
            current = Some(step.trim_end_matches(['\r', '\n']));
            &line[prefix.len()..]
        } else {
            line
        };
        if current == Some(selected) {
            out.push_str(rest);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::gate_lifecycle::GateStep;
    use crate::operation::gate_process::{
        GateEvidence, render_gate_wrapper, verdict_from_exit_code,
    };
    use calm_types::observation::Observation;
    use std::io::Write;

    #[test]
    fn output_without_a_final_newline_does_not_move_the_failure_or_hide_its_step_reference() {
        let dir = tempfile::tempdir().unwrap();
        let evidence = GateEvidence {
            log_path: dir.path().join("gate.log"),
            step_path: Some(dir.path().join("gate.step")),
            steps: vec![
                GateStep {
                    name: "build".into(),
                    cmd: "printf 'progress 100%%'".into(),
                },
                GateStep {
                    name: "test".into(),
                    cmd: "echo boom; exit 4".into(),
                },
            ],
        };
        let script = dir.path().join("gate.sh");
        std::fs::write(&script, render_gate_wrapper(&evidence.steps)).unwrap();
        let mut child = std::process::Command::new("/bin/sh")
            .arg(script)
            .current_dir(dir.path())
            .env("NEIGE_GATE_STEP_PATH", evidence.step_path.as_ref().unwrap())
            .env("NEIGE_GATE_EXIT_PATH", dir.path().join("gate.exit"))
            .env("NEIGE_GATE_LOG_PATH", &evidence.log_path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"go\n").unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(4));
        let verdict = verdict_from_exit_code(4, &evidence, 1);
        assert!(!verdict.passed);
        assert_eq!(verdict.failing_step.as_deref(), Some("test"));
        let wake = Observation::TaskGateResult {
            idempotency_key: "attempt".into(),
            key: "task".into(),
            passed: verdict.passed,
            failing_step: verdict.failing_step.clone(),
            exit_code: verdict.exit_code,
            log_tail: verdict.log_tail,
            attempt: 1,
            status_detail: verdict.status_detail,
            target: None,
        }
        .to_turn_text();
        assert!(wake.contains("neige track cat runs/attempt/gates/1.log --step 'test'"));
        let log = std::fs::read_to_string(&evidence.log_path).unwrap();
        assert_eq!(step_text(&log, "test"), "::gate-step test\nboom\n");
        assert_eq!(step_text(&log, "build"), "::gate-step build\nprogress 100%");
    }
}
