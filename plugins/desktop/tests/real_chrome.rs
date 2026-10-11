//! The fence with a real Chrome, driven the way a steered agent would: type a
//! `file://` URL into the address bar with `window_input`, then read the tab
//! with `page_cat`. Inside the sandbox the host's directories do not exist, so
//! Chrome shows its error page, never a listing.
//!
//! Needs a real Chrome, so CI skips it. Run it with
//! `DESKTOP_TEST_CHROME=<chrome binary> cargo nextest run -p desktop
//! --run-ignored only a_typed_file_url`.
#![cfg(target_os = "linux")]

mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::*;

#[test]
#[ignore = "needs a real Chrome: set DESKTOP_TEST_CHROME to its binary"]
fn a_typed_file_url_reads_no_host_directory() {
    let chrome = std::env::var("DESKTOP_TEST_CHROME")
        .expect("DESKTOP_TEST_CHROME names the Chrome binary to test");
    let tmp = tmp();
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let sentinel = "sentinel-the-sandbox-must-hide.txt";
    std::fs::write(outside.join(sentinel), b"secret").unwrap();
    // An English UI, so a listing is titled "Index of".
    let mut plugin = Plugin::spawn_env(
        &tmp.path().join("desktop"),
        &[("LANG", "C"), ("LC_ALL", "C")],
    );
    assert!(plugin.initialize(&chrome).get("result").is_some());
    // Starts Chrome. Nothing listens on the discard port, so the load fails.
    let _ = plugin.tool("page_open", json!({ "url": "http://127.0.0.1:9/" }));
    let window = chrome_window(&mut plugin);
    // Let the window draw before typing, as an agent's `window_cat` does: keys
    // sent to a window Chrome has not focused yet are lost.
    plugin
        .tool("window_cat", json!({ "window_id": window }))
        .unwrap();

    let mut dirs: Vec<PathBuf> = vec![outside.clone()];
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    for dir in [home.as_path(), Path::new("/mnt/data2")] {
        if dir.is_dir() {
            dirs.push(dir.to_path_buf());
        }
    }
    for dir in &dirs {
        let url = format!("file://{}/", dir.display());
        // Typed again when the address bar missed it; three tries.
        let page = (0..3)
            .find_map(|_| {
                let typed = plugin
                    .tool(
                        "window_input",
                        json!({ "window_id": window, "events": [
                            { "type": "key", "keys": "ctrl+l" },
                            { "type": "text", "text": format!("{url}\n") },
                        ] }),
                    )
                    .unwrap();
                assert_eq!(typed["structuredContent"], json!({ "ok": true }));
                page_at(&mut plugin, &url)
            })
            .unwrap_or_else(|| panic!("{url} never showed"));
        let text = page["text"].as_str().unwrap();
        eprintln!("{url}: title {:?}, text {:?}", page["title"], text);
        assert!(!text.contains("Index of"), "{url} listed: {text}");
        assert!(!text.contains(sentinel), "{url} listed: {text}");
        assert!(text.contains("ERR_FILE_NOT_FOUND"), "{url}: {text}");
    }
}

/// The id of Chrome's window, once it has one.
fn chrome_window(plugin: &mut Plugin) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let ls = plugin.tool("window_ls", json!({})).unwrap()["structuredContent"].clone();
        let found = ls["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["app"] == "chrome")
            .and_then(|w| w["window_id"].as_u64());
        if let Some(id) = found {
            return id;
        }
        assert!(Instant::now() < deadline, "no Chrome window: {ls}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// `page_cat` once the visible tab shows `url`: a listing at that url, or
/// Chrome's error page (`chrome-error://chromewebdata/`) titled with it.
/// `None` when it does not within 7 s.
fn page_at(plugin: &mut Plugin, url: &str) -> Option<Value> {
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        let page = plugin.tool("page_cat", json!({}));
        if let Ok(page) = &page {
            let page = &page["structuredContent"];
            let shown = page["url"] == url || page["title"] == url;
            if shown && !page["text"].as_str().unwrap_or_default().is_empty() {
                return Some(page.clone());
            }
        }
        if Instant::now() >= deadline {
            eprintln!("{url} not shown yet: {page:?}");
            return None;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}
