//! How the plugin shows an [`ArchiveStatus`]: the `archive.status` table
//! overlay and the tool's one-paragraph text.

use serde_json::{Value, json};

use crate::status::{ArchiveStatus, Health, RunStatus, time};

/// The overlay kind a Report names as
/// `neige://plugin/research-archive/archive.status`.
pub const OVERLAY_KIND: &str = "archive.status";

/// The report `table` block's cap on one string, in characters.
const MAX_CAPTION_CHARS: usize = 2048;

/// The status as a report `table` payload: one row per recent run, and the
/// verdict with its reasons in the caption.
pub fn table(status: &ArchiveStatus) -> Value {
    let rows: Vec<Value> = status.runs.iter().map(row).collect();
    json!({
        "columns": [
            { "key": "as_of", "label": "Trading day" },
            { "key": "outcome", "label": "Outcome" },
            { "key": "ok", "label": "OK", "align": "right" },
            { "key": "failed", "label": "Failed", "align": "right" },
            { "key": "untried", "label": "Not tried", "align": "right" },
            { "key": "planned", "label": "Planned", "align": "right" },
            { "key": "membership", "label": "Membership" },
            { "key": "finished", "label": "Finished (UTC)" },
        ],
        "rows": rows,
        "caption": capped(&headline(status)),
    })
}

fn row(run: &RunStatus) -> Value {
    let membership: Vec<String> = run
        .frozen
        .iter()
        .map(|f| format!("{} {}", f.index, f.mode))
        .collect();
    json!({
        "as_of": run.as_of,
        "outcome": run.outcome,
        "ok": run.pairs_ok,
        "failed": run.pairs_failed,
        "untried": run.pairs_untried,
        "planned": run.pairs_planned,
        "membership": if membership.is_empty() { "not frozen".to_owned() } else { membership.join(", ") },
        "finished": run.finished_at.map(time),
    })
}

/// The verdict, its reasons and the last start, in one paragraph.
pub fn headline(status: &ArchiveStatus) -> String {
    let verdict = match status.health {
        Health::Ok => "Healthy",
        Health::Attention => "Attention",
        Health::Failing => "FAILING",
    };
    let mut text = verdict.to_owned();
    for finding in &status.findings {
        text.push_str(&format!(" · {}", finding.text));
    }
    if let Some(last) = &status.last_exit {
        text.push_str(&format!(
            " · last capture start: {} at {}",
            last.kind.as_str(),
            time(last.at)
        ));
    }
    // No check time: the table changes only when the status does, so the
    // overlay is pushed only then.
    text
}

fn capped(text: &str) -> String {
    if text.chars().count() <= MAX_CAPTION_CHARS {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(MAX_CAPTION_CHARS - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Finding;

    #[test]
    fn the_caption_fits_the_table_block() {
        let status = ArchiveStatus {
            checked_at: 0,
            health: Health::Failing,
            findings: vec![
                Finding {
                    health: Health::Failing,
                    text: "é".repeat(MAX_CAPTION_CHARS),
                };
                2
            ],
            last_exit: None,
            runs: Vec::new(),
        };
        let table = table(&status);
        let caption = table["caption"].as_str().unwrap();
        assert_eq!(caption.chars().count(), MAX_CAPTION_CHARS);
        assert!(caption.starts_with("FAILING · é") && caption.ends_with("é…"));
        calm_types::report_blocks::kinds::validate_inline_table_overlay(&table).unwrap();
        assert_eq!(
            MAX_CAPTION_CHARS,
            calm_types::report_blocks::kinds::MAX_STRING_CHARS
        );
    }
}
