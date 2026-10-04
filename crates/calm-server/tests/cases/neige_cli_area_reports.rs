//! #1838 S2 end to end through the real `neige` path (kernel socket, `neige/cli`): the issue's
//! `tag -> ls / ls -l / --json / find -> cat` loop over `area/reports/`, the ambiguous-name refusal,
//! and the Worker's Forbidden. Run with `--no-capture` to see the transcript it prints.

#![cfg(unix)]

use crate::support;

use calm_server::ids::{AreaId, TrackId};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, TrackPatch};
use calm_server::track_report::TrackReportPayload;
use chrono::{Local, SecondsFormat, TimeZone};
use serde_json::{Value, json};
use support::mcp::{CardBoot, boot_with_role, cli_output, neige_cli_via_socket};

/// Runs one argv and prints it with its output, like a terminal session.
async fn neige(boot: &CardBoot, argv: &[&str]) -> (String, String, i64) {
    let out = cli_output(&neige_cli_via_socket(&boot.socket_path, &boot.raw_token, argv).await);
    let shown: Vec<String> = argv
        .iter()
        .map(|arg| {
            if arg.contains([' ', '*', '?']) {
                format!("'{arg}'")
            } else {
                arg.to_string()
            }
        })
        .collect();
    println!(
        "$ neige {}\n{}{}[exit {}]",
        shown.join(" "),
        out.0,
        out.1,
        out.2
    );
    out
}

async fn ok(boot: &CardBoot, argv: &[&str]) -> String {
    let (stdout, stderr, exit) = neige(boot, argv).await;
    assert_eq!((exit, stderr.as_str()), (0, ""), "{argv:?}");
    stdout
}

async fn area_of(boot: &CardBoot) -> AreaId {
    boot.repo
        .track_get(boot.track_id.as_str())
        .await
        .unwrap()
        .unwrap()
        .area_id
}

async fn add_track(boot: &CardBoot, area: &AreaId, title: &str) -> TrackId {
    boot.repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.clone(),
            title: title.into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap()
        .id
}

/// The report card every production track is created with, then `body` through the persist boundary.
async fn add_report(boot: &CardBoot, track: &TrackId, body: &str) -> String {
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: track.clone(),
            title: None,
            kind: "track-report".into(),
            sort: Some(-1.0),
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
    let track = boot.repo.track_get(track.as_str()).await.unwrap().unwrap();
    let write = calm_server::state::WriteContext::new(
        boot.card_role_cache.clone(),
        boot.track_area_cache.clone(),
    );
    let current: TrackReportPayload = serde_json::from_value(card.payload.clone()).unwrap();
    let card = calm_server::track_report::persist_report(
        boot.sqlx.as_ref(),
        &boot.events,
        &write,
        calm_server::ids::ActorId::Kernel,
        calm_server::event::EditAuthor::Planner,
        track,
        card,
        current,
        TrackReportPayload::new("", body),
        0,
        None,
    )
    .await
    .expect("persist report body");
    card.id.as_str().to_string()
}

/// Tag another track's report through the production tag writer (the CLI only tags its own).
async fn tag_other(boot: &CardBoot, track: &TrackId, add: &[&str]) {
    let add: Vec<String> = add.iter().map(|tag| tag.to_string()).collect();
    let mut tx = boot.sqlx.pool().begin().await.unwrap();
    calm_server::report_tags::store::apply_tx(&mut tx, track.as_str(), &add, &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

fn local_ms(hour: u32, minute: u32) -> i64 {
    Local
        .with_ymd_and_hms(2026, 9, 28, hour, minute, 0)
        .single()
        .unwrap()
        .timestamp_millis()
}

async fn set_report_time(boot: &CardBoot, card: &str, at: i64) {
    sqlx::query("UPDATE cards SET updated_at = ?1 WHERE id = ?2")
        .bind(at)
        .bind(card)
        .execute(boot.sqlx.pool())
        .await
        .unwrap();
}

const AUTH_BODY: &str = "# 认证 方案\n\n结论：采用 OIDC，会话 8 小时。\n";
const LOGIN_BODY: &str = "# 登录 排查\n\n结论：过期 会话 未刷新。\n";

#[tokio::test]
async fn planner_browses_searches_and_reads_area_reports_through_neige() {
    let boot = boot_with_role(CardRole::Planner).await;
    let area = area_of(&boot).await;
    boot.repo
        .track_update(
            boot.track_id.as_str(),
            TrackPatch {
                title: Some("认证 方案".into()),
                ..TrackPatch::default()
            },
        )
        .await
        .unwrap();
    let own_report = add_report(&boot, &boot.track_id, AUTH_BODY).await;
    let login = add_track(&boot, &area, "登录 排查").await;
    let login_report = add_report(&boot, &login, LOGIN_BODY).await;
    let elsewhere = boot
        .repo
        .area_create(NewArea {
            name: "elsewhere".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let foreign = add_track(&boot, &elsewhere.id, "认证 方案").await;
    add_report(&boot, &foreign, "another area's report\n").await;
    tag_other(&boot, &foreign, &["认证"]).await;

    assert_eq!(
        ok(
            &boot,
            &[
                "report",
                "tag",
                "report.md",
                "--add",
                "认证",
                "--add",
                "架构"
            ]
        )
        .await,
        "认证 架构\n"
    );
    tag_other(&boot, &login, &["认证", "排障"]).await;
    set_report_time(&boot, &own_report, local_ms(14, 30)).await;
    set_report_time(&boot, &login_report, local_ms(10, 15)).await;

    assert_eq!(ok(&boot, &["track", "ls", "area/"]).await, "d reports/\n");
    assert_eq!(
        ok(&boot, &["track", "ls", "area/reports/"]).await,
        "认证 方案.md\n登录 排查.md\n"
    );
    assert_eq!(
        ok(&boot, &["track", "ls", "-l", "area/reports/"]).await,
        "UPDATED_AT        TAGS       NAME\n\
         2026-09-28 14:30  认证,架构  认证 方案.md\n\
         2026-09-28 10:15  认证,排障  登录 排查.md\n"
    );
    let rfc = |ms: i64| {
        Local
            .timestamp_millis_opt(ms)
            .single()
            .unwrap()
            .to_rfc3339_opts(SecondsFormat::Millis, false)
    };
    let listed: Value =
        serde_json::from_str(&ok(&boot, &["--json", "track", "ls", "area/reports/"]).await)
            .unwrap();
    assert_eq!(
        listed,
        json!([
            { "path": "area/reports/认证 方案.md", "title": "认证 方案", "trackId": boot.track_id.as_str(),
              "tags": ["认证", "架构"], "updatedAt": rfc(local_ms(14, 30)) },
            { "path": "area/reports/登录 排查.md", "title": "登录 排查", "trackId": login.as_str(),
              "tags": ["认证", "排障"], "updatedAt": rfc(local_ms(10, 15)) }
        ])
    );

    assert_eq!(
        ok(
            &boot,
            &["report", "find", "area/reports/", "--name", "*认证*"]
        )
        .await,
        "area/reports/认证 方案.md\n"
    );
    assert_eq!(
        ok(&boot, &["report", "find", "area/reports/", "--tag", "认证"]).await,
        "area/reports/认证 方案.md\narea/reports/登录 排查.md\n"
    );
    assert_eq!(
        ok(
            &boot,
            &[
                "report",
                "find",
                "area/reports/",
                "--name",
                "*排查*",
                "--tag",
                "认证"
            ]
        )
        .await,
        "area/reports/登录 排查.md\n"
    );
    assert_eq!(
        ok(
            &boot,
            &["report", "find", "area/reports/", "--tag", "不存在"]
        )
        .await,
        ""
    );
    assert_eq!(
        ok(
            &boot,
            &[
                "--json",
                "report",
                "find",
                "area/reports",
                "--tag",
                "不存在"
            ]
        )
        .await,
        "[]\n"
    );

    assert_eq!(
        ok(&boot, &["track", "cat", "area/reports/登录 排查.md"]).await,
        LOGIN_BODY
    );
    assert_eq!(
        ok(&boot, &["track", "cat", "area/reports/认证 方案.md"]).await,
        AUTH_BODY
    );
    assert_eq!(
        ok(&boot, &["track", "cat", "report.md"]).await,
        AUTH_BODY,
        "report.md is unchanged"
    );

    // A tag change is a report change: the listing's time moves off 14:30.
    ok(&boot, &["report", "tag", "report.md", "--add", "排障"]).await;
    let long = ok(&boot, &["track", "ls", "-l", "area/reports/"]).await;
    let own_line = long.lines().find(|l| l.ends_with("认证 方案.md")).unwrap();
    assert!(own_line.contains("  认证,架构,排障  "), "{long}");
    assert!(!own_line.starts_with("2026-09-28 14:30"), "{long}");

    // A second track with the same title: both list suffixed, the bare name is refused with both.
    let twin = add_track(&boot, &area, "认证 方案").await;
    add_report(&boot, &twin, "# 认证 方案\n\n第二份 结论。\n").await;
    let names = ok(&boot, &["track", "ls", "area/reports/"]).await;
    let suffixed: Vec<String> = names
        .lines()
        .filter(|name| name.starts_with("认证 方案~"))
        .map(|name| format!("area/reports/{name}"))
        .collect();
    assert_eq!(suffixed.len(), 2, "{names}");
    let (stdout, stderr, exit) = neige(&boot, &["track", "cat", "area/reports/认证 方案.md"]).await;
    assert_eq!((exit, stdout.as_str()), (4, ""));
    assert!(
        stderr.starts_with("neige: neige_track_cat: `area/reports/认证 方案.md` names 2 reports in this area; read one of: "),
        "{stderr}"
    );
    assert!(stderr.ends_with(" (code -32602)\n"), "{stderr}");
    for path in &suffixed {
        assert!(stderr.contains(path.as_str()), "{stderr}");
    }
    let twin_path = suffixed
        .iter()
        .find(|path| path.contains(&twin.as_str()[..8]))
        .unwrap();
    assert_eq!(
        ok(&boot, &["track", "cat", twin_path]).await,
        "# 认证 方案\n\n第二份 结论。\n"
    );

    // A rename moves the path; a title with a space is one quoted argument.
    boot.repo
        .track_update(
            twin.as_str(),
            TrackPatch {
                title: Some("认证 方案 第二版".into()),
                ..TrackPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        ok(&boot, &["track", "cat", "area/reports/认证 方案 第二版.md"]).await,
        "# 认证 方案\n\n第二份 结论。\n"
    );
    assert_eq!(
        ok(&boot, &["track", "cat", "area/reports/认证 方案.md"]).await,
        AUTH_BODY
    );
}

#[tokio::test]
async fn a_worker_is_refused_area_reports_through_neige() {
    let boot = boot_with_role(CardRole::Worker).await;
    add_report(&boot, &boot.track_id, "own\n").await;
    for argv in [
        &["track", "ls", "area/reports/"][..],
        &["track", "cat", "area/reports/mcp-test.md"][..],
        &["report", "find", "area/reports/", "--tag", "x"][..],
    ] {
        let (stdout, stderr, exit) = neige(&boot, argv).await;
        assert_eq!((exit, stdout.as_str()), (4, ""), "{argv:?}");
        assert!(
            stderr.contains("area/reports/ is the Planner's view")
                && stderr.ends_with("(code -32403)\n"),
            "{argv:?}: {stderr}"
        );
    }
    assert_eq!(ok(&boot, &["track", "cat", "report.md"]).await, "own\n");
}

const BLOCKS_BODY: &str =
    "Contract intro.\n\n# Goal\n\nalpha\n\n# Findings\n\nbeta\n\n# Next\n\ngamma\n";

/// The report's block ids in document order, from the snapshot `neige_area_outline` also reads.
async fn block_ids(boot: &CardBoot, report_card: &str) -> Vec<String> {
    calm_server::track_report_read::load_report_doc_snapshot(boot.repo.as_ref(), report_card)
        .await
        .unwrap()
        .blocks
        .into_iter()
        .map(|block| block.id)
        .collect()
}

/// #1874 through the real `neige` path: `cat <report> --blocks` prints the chosen blocks with their
/// marker lines, the same bytes under `--json`, and a refusal exits 4.
#[tokio::test]
async fn planner_reads_chosen_report_blocks_through_neige() {
    let boot = boot_with_role(CardRole::Planner).await;
    let area = area_of(&boot).await;
    let login = add_track(&boot, &area, "登录 排查").await;
    let ids = block_ids(&boot, &add_report(&boot, &login, BLOCKS_BODY).await).await;
    assert_eq!(ids.len(), 4, "{ids:?}");

    let path = "area/reports/登录 排查.md";
    let chosen = format!("{},{}", ids[3], ids[1]);
    let want = format!(
        "<!-- neige:{} -->\n# Goal\n\nalpha\n\n<!-- neige:{} -->\n# Next\n\ngamma\n",
        ids[1], ids[3]
    );
    assert_eq!(
        ok(&boot, &["track", "cat", path, "--blocks", &chosen]).await,
        want
    );
    assert_eq!(
        ok(
            &boot,
            &["--json", "track", "cat", "--blocks", &chosen, path]
        )
        .await,
        want
    );
    let (stdout, stderr, exit) = neige(&boot, &["track", "cat", path, "--blocks", "b_nope"]).await;
    assert_eq!((exit, stdout.as_str()), (4, ""));
    assert!(stderr.contains("unknown block id `b_nope`"), "{stderr}");

    // #1877: --sections names whole H1 sections and prints the same bytes.
    assert_eq!(
        ok(&boot, &["track", "cat", path, "--sections", "Next,Goal"]).await,
        want
    );
    let (stdout, stderr, exit) = neige(&boot, &["track", "cat", path, "--sections", "Nope"]).await;
    assert_eq!((exit, stdout.as_str()), (4, ""));
    assert!(
        stderr.contains("unknown section `Nope`; this report's sections are:\n  # Goal"),
        "{stderr}"
    );
}
