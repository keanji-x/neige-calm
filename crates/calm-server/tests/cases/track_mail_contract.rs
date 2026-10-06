//! #2160 mail input validation and structured CLI results, through the real MCP boundary.

use serde_json::{Value, json};

use super::track_mail_fixture::{World, mail_and_hop, refusal};

/// No recorded turn and a refused hop are distinct structured results.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_cat_without_a_turn_is_not_a_hop_refusal() {
    let w = World::new(&["A", "B"]).await;
    let (a, b) = (w.p(0), w.p(1));
    let mail = w.seed_mail(&a.track_id, &b.track_id, 6).await;
    let cat = w.cat_json(b, &mail).await;
    assert_eq!(cat["next_hop"], Value::Null);
    assert_eq!(cat["refused"], json!(false));
    let (stdout, stderr, exit) = w.neige(b, &["mail", "cat", &mail]).await;
    assert_eq!(exit, 0, "{stderr}");
    assert!(stdout.ends_with("\n\nseeded body\n"), "{stdout}");
}

/// Argument validation runs through the real MCP boundary and emits no mail or wake on refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_send_refuses_summary_controls_before_trimming() {
    let w = World::new(&["R", "N"]).await;
    let (r, n) = (w.p(0), w.p(1));
    w.user_turn(r, "ask N").await;
    for c in [
        '\n', '\r', '\t', '\u{000b}', '\u{001b}', '\u{0085}', '\u{2028}', '\u{2029}',
    ] {
        for summary in [format!("a{c}b"), format!("{c}ab"), format!("ab{c}")] {
            let error = w
                .send(
                    r,
                    json!({
                        "track_id": n.track_id.as_str(), "summary": summary, "text": "body"
                    }),
                )
                .await
                .expect_err("single-line summary");
            assert_eq!(
                refusal(&error, "summary"),
                (
                    -32602,
                    "neige_mail_send: summary is 1..200 characters on one line".into()
                )
            );
        }
    }
    assert_eq!((w.mail_rows().await, w.wake_events().await), (0, 0));
    let result = w
        .send(
            r,
            json!({
                "track_id": n.track_id.as_str(), "summary": "  中文 résumé  ", "text": "a\nb\t"
            }),
        )
        .await
        .unwrap();
    let (mail_id, _) = mail_and_hop(&result);
    let stored: (String, String) = sqlx::query_as("SELECT summary, text FROM mails WHERE id = ?1")
        .bind(mail_id)
        .fetch_one(w.repo.pool())
        .await
        .unwrap();
    assert_eq!(stored, ("中文 résumé".into(), "a\nb\t".into()));
}
