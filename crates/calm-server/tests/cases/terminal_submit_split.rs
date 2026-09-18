//! #1725 — `submit` through the real MCP tools, renderer, supervisor and
//! PTY: the program reads the text, then the CR as its own `read(2)`; one
//! input sequence, one receipt, and a replay writes nothing.
use crate::terminal_support::Harness;
use serde_json::{Value, json};
use std::time::Duration;

/// Raw input (VMIN 1, no echo) with output processing kept on, so every
/// `read(2)` is one `od -An -c` block and its rows stay left-aligned. A read
/// that carried the text and the CR together renders `\r` at the end of the
/// text's last row; a CR that arrived as its own read is a row of exactly
/// `\r`. `READY` marks the moment `dd` is about to block in `read(2)`.
const READ_PROBE: &str = "stty raw -echo opost; echo READY; while :; do dd bs=4096 count=1 2>/dev/null | od -An -c; done";

/// The rows `od -An -c` prints for one read of `bytes`, trimmed like the
/// observation's rows: 16 bytes per row, a printable ASCII byte as itself, a
/// CR as `\r`, any other byte as three octal digits. A length that is a
/// multiple of 16 would put a CR read TOGETHER with the text on a row of its
/// own, so the probe texts avoid it (asserted where they are built).
fn od_rows(bytes: &[u8]) -> Vec<String> {
    bytes
        .chunks(16)
        .map(|row| {
            row.iter()
                .map(|byte| match byte {
                    b'\r' => "  \\r".to_owned(),
                    0x20..=0x7e => format!("   {}", *byte as char),
                    other => format!(" {other:03o}"),
                })
                .collect::<String>()
                .trim()
                .to_owned()
        })
        .collect()
}
fn rows(state: &Value) -> Vec<String> {
    state["text"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().trim().to_owned())
        .collect()
}
fn receipt(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    &response["result"]["structuredContent"]
}
fn error_text(response: &Value) -> String {
    response["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an error: {response}"))
        .to_owned()
}
/// A row of exactly `\r` whose predecessor is the text's last row: the CR
/// arrived as its own read AFTER a read that ended with the text.
fn cr_read_alone(rows: &[String], last_text_row: &str) -> bool {
    rows.windows(2)
        .any(|pair| pair[0] == last_text_row && pair[1] == "\\r")
}
async fn open_probe(h: &Harness, request: &str) -> String {
    let opened = h
        .ok(
            "calm.terminal.open",
            json!({"program":READ_PROBE,"request_id":request,"claim":true,
                "wait_for":"text","wait_text":["READY"],"wait_ms":5000}),
        )
        .await;
    assert_eq!(opened["claim"]["status"], "claimed", "{opened}");
    assert_eq!(opened["wait"]["outcome"], "matched", "{opened}");
    // `dd` forks right after READY; give it the moment it needs to block in
    // `read(2)` so the first read is the one waiting for the text.
    tokio::time::sleep(Duration::from_millis(200)).await;
    opened["terminal_id"].as_str().unwrap().to_owned()
}
/// Submits `text`, asserts the receipt and the single sequence step, waits
/// until the text's last row and the CR octet are on the screen (whichever
/// read carried the CR), lets the output settle and returns the rows of a
/// fresh observation (which is also the latest observation the next input
/// will act on).
async fn submit_and_read(h: &Harness, terminal: &str, request: &str, text: &str) -> Vec<String> {
    let before = h.interaction().input_ack_sequence(terminal).await.unwrap();
    let sent = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":request,"action":{"type":"submit","text":text},
                "observe":true,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    let written = receipt(&sent);
    assert_eq!(written["outcome"], "written", "{sent}");
    assert!(written.get("steps").is_none(), "{written}");
    assert!(written.get("replace").is_none(), "{written}");
    assert_eq!(
        h.interaction().input_ack_sequence(terminal).await,
        Some(before + 1),
        "text and CR: one acknowledged input sequence"
    );
    let expected = od_rows(text.as_bytes());
    let last_text_row = expected.last().unwrap().as_str();
    let started = std::time::Instant::now();
    loop {
        let view = h
            .ok(
                "calm.terminal.observe",
                json!({"terminal_id":terminal,"wait_ms":0}),
            )
            .await;
        let seen = rows(&view);
        // Whichever read carried the CR: alone on its row, or at the end
        // of the text's last row.
        let text_landed = seen.iter().any(|row| row.starts_with(last_text_row));
        let cr_landed = seen.iter().any(|row| row.ends_with("\\r"));
        if text_landed && cr_landed {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the probe never showed the text and the CR: {view}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let view = h
        .ok(
            "calm.terminal.observe",
            json!({"terminal_id":terminal,"wait_ms":0}),
        )
        .await;
    rows(&view)
}
/// 300 CJK characters (900 bytes): the size the issue measured as a paste
/// when the CR arrived in the same read.
fn cjk_text() -> String {
    const DIGITS: [&str; 10] = [
        "\u{4e00}", "\u{4e8c}", "\u{4e09}", "\u{56db}", "\u{4e94}", "\u{516d}", "\u{4e03}",
        "\u{516b}", "\u{4e5d}", "\u{5341}",
    ];
    let text: String = (0..300).map(|index| DIGITS[index % DIGITS.len()]).collect();
    assert_eq!(text.chars().count(), 300);
    assert_eq!(text.len(), 900);
    text
}

/// Over up to three submits ("hello", the 300-character CJK text, "hello"
/// again), at least one shows the CR as its own read: a row of exactly `\r`
/// right after the text's last row, which carries no `\r`. Before #1725
/// every read showed the text and the CR together (`h e l l o \r` on one
/// row, never a lone `\r` row), so 0 of 3 is the decisive red; after it a
/// false red needs the reader to lose the `SUBMIT_CR_GAP` race three times
/// in a row.
#[tokio::test]
async fn submit_reaches_the_pty_as_text_then_the_cr_as_its_own_read() {
    let h = Harness::start().await;
    let terminal = open_probe(&h, "submit-split").await;
    let cjk = cjk_text();
    let attempts = [("split-hello", "hello"), ("split-cjk", cjk.as_str())];
    let mut split = 0;
    for (request, text) in attempts {
        assert_ne!(
            text.len() % 16,
            0,
            "a text of 16n bytes cannot tell the reads apart"
        );
        let seen = submit_and_read(&h, &terminal, request, text).await;
        let expected = od_rows(text.as_bytes());
        let last_text_row = expected.last().unwrap();
        let alone = cr_read_alone(&seen, last_text_row);
        let shown: Vec<&String> = seen.iter().filter(|row| !row.is_empty()).collect();
        println!("PROBE {request}: cr_read_alone={alone} rows={shown:?}");
        if alone {
            split += 1;
        }
    }
    if split == 0 {
        let seen = submit_and_read(&h, &terminal, "split-hello-again", "hello").await;
        let alone = cr_read_alone(&seen, od_rows(b"hello").last().unwrap());
        println!("PROBE split-hello-again: cr_read_alone={alone} rows={seen:?}");
        if alone {
            split += 1;
        }
    }
    assert!(
        split >= 1,
        "no submit reached the PTY as text, then the CR as its own read"
    );
    h.stop(&terminal).await;
}

/// One submit is ONE request: the acknowledged input sequence advances by
/// exactly one for the two physical writes; the receipt is `written` with
/// no `steps`; a replay of the request id returns the cached receipt and
/// writes nothing (neither the sequence nor the screen moves); a replay
/// with another text is refused as reused arguments.
#[tokio::test]
async fn submit_is_one_sequence_one_receipt_and_a_replay_writes_nothing() {
    let h = Harness::start().await;
    let terminal = open_probe(&h, "submit-once").await;
    let before = h.interaction().input_ack_sequence(&terminal).await.unwrap();
    let seen = submit_and_read(&h, &terminal, "once", "hello").await;
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before + 1)
    );
    let hello_rows = |rows: &[String]| {
        rows.iter()
            .filter(|row| row.starts_with("h   e   l   l   o"))
            .count()
    };
    let cr_rows = |rows: &[String]| rows.iter().filter(|row| row.ends_with("\\r")).count();
    assert_eq!(hello_rows(&seen), 1, "{seen:?}");
    assert_eq!(cr_rows(&seen), 1, "{seen:?}");
    let replay = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"once","action":{"type":"submit","text":"hello"},
                "observe":true,"wait_ms":300}),
        )
        .await;
    let cached = receipt(&replay);
    assert_eq!(cached["outcome"], "written", "{replay}");
    assert!(cached.get("steps").is_none(), "{cached}");
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before + 1),
        "a replay reserves no sequence"
    );
    let after = rows(&cached["observation"]["state"]);
    assert_eq!(hello_rows(&after), 1, "no second copy: {after:?}");
    assert_eq!(cr_rows(&after), 1, "no second CR: {after:?}");
    let conflicting = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"once","action":{"type":"submit","text":"hello again"}}),
        )
        .await;
    assert!(
        error_text(&conflicting).contains("reused with different arguments"),
        "{conflicting}"
    );
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before + 1)
    );
    h.stop(&terminal).await;
}
