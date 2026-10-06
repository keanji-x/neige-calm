//! #2087 C2 (§5): `neige_area_ls` pages over the area's tracks by `cursor` and never drops a row.
//! A page ends at 50 tracks or early at its byte budget; a report's blocks past 40 are counted in
//! the row's `blocks_truncated`, never silently clipped.

use super::*;
use calm_server::mcp_server::tools::report_links::TOOL_AREA_LS;

/// Track rows several kilobytes long: 45 sections whose headings are 60 three-byte characters.
fn long_body(index: usize) -> String {
    (0..45)
        .map(|section| format!("# {index:02}{section:02}{}\n\nbody\n\n", "节".repeat(56)))
        .collect()
}

#[tokio::test]
async fn area_ls_pages_end_at_the_byte_budget_and_lose_no_track() {
    let titles: Vec<String> = (0..7).map(|index| format!("track {index}")).collect();
    let specs: Vec<(usize, &str)> = titles.iter().map(|title| (0, title.as_str())).collect();
    let boot = boot(&specs).await;
    for (index, side) in boot.sides.iter().enumerate().skip(1) {
        write_body(&boot, side, &long_body(index)).await;
    }
    let reader = &boot.sides[0];

    let (mut seen, mut cursor, mut sizes) = (Vec::new(), None::<String>, Vec::new());
    loop {
        let args = cursor
            .as_ref()
            .map_or(json!({}), |c| json!({ "cursor": c }));
        let page = call(&boot, TOOL_AREA_LS, planner(reader), args)
            .await
            .expect("neige_area_ls");
        assert!(
            page.get("truncated").is_none(),
            "no top-level truncated: {page}"
        );
        let tracks = page["tracks"].as_array().expect("tracks");
        let row_bytes: usize = tracks
            .iter()
            .map(|row| serde_json::to_vec(row).unwrap().len() + 1)
            .sum();
        assert!(
            row_bytes <= 32 * 1024 || tracks.len() == 1,
            "{row_bytes} bytes"
        );
        sizes.push(tracks.len());
        for row in tracks {
            let blocks = row["blocks"].as_array().expect("blocks").len();
            let clipped = row["blocks_truncated"].as_u64().expect("blocks_truncated");
            if row["track_id"] == reader.track_id.as_str() {
                assert_eq!(clipped, 0, "{row}");
            } else {
                assert_eq!(
                    (blocks, clipped),
                    (40, 5),
                    "45 sections: 40 listed, 5 counted"
                );
            }
            seen.push(row["track_id"].as_str().unwrap().to_string());
        }
        match &page["next_cursor"] {
            Value::String(next) => {
                assert_eq!(Some(next.as_str()), seen.last().map(String::as_str));
                cursor = Some(next.clone());
            }
            Value::Null => break,
            other => panic!("next_cursor {other}"),
        }
    }
    assert!(
        sizes.len() > 1 && sizes[0] < 50,
        "the byte budget, not the row count, ended the first page: {sizes:?}"
    );
    let mut expected: Vec<String> = boot
        .sides
        .iter()
        .map(|side| side.track_id.as_str().to_string())
        .collect();
    expected.sort();
    assert_eq!(seen, expected, "every track once, in track id order");

    let refused = call(
        &boot,
        TOOL_AREA_LS,
        planner(reader),
        json!({ "cursor": "zzzz-not-a-track" }),
    )
    .await
    .expect_err("a cursor that names no track is refused");
    assert_eq!(refused.code, INVALID_PARAMS, "{refused:?}");
    assert!(
        refused.message.contains("names no row of this listing"),
        "{refused:?}"
    );
}
