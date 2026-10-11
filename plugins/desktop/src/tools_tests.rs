use super::*;

const SIZE: (u32, u32) = (1280, 800);

fn events(v: Value) -> Result<Vec<EventArg>, String> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

#[test]
fn input_arguments_parse_each_event_shape_and_refuse_unknown_keys() {
    let parsed = events(json!([
        { "type": "click", "x": 1, "y": 2 },
        { "type": "click", "x": 1, "y": 2, "button": "right" },
        { "type": "move", "x": 3.5, "y": 4 },
        { "type": "scroll", "x": 5, "y": 6, "dx": 0, "dy": -120 },
        { "type": "key", "keys": "ctrl+l" },
        { "type": "text", "text": "hi" },
    ]))
    .unwrap();
    assert_eq!(
        parsed,
        [
            EventArg::Click {
                x: 1.0,
                y: 2.0,
                button: Button::Left
            },
            EventArg::Click {
                x: 1.0,
                y: 2.0,
                button: Button::Right
            },
            EventArg::Move { x: 3.5, y: 4.0 },
            EventArg::Scroll {
                x: 5.0,
                y: 6.0,
                dx: 0.0,
                dy: -120.0
            },
            EventArg::Key {
                keys: "ctrl+l".into()
            },
            EventArg::Text { text: "hi".into() },
        ]
    );
    for bad in [
        json!([{ "type": "click", "x": 1 }]),
        json!([{ "type": "click", "x": 1, "y": 2, "z": 3 }]),
        json!([{ "type": "click", "x": 1, "y": 2, "button": "back" }]),
        json!([{ "type": "key", "keys": "a", "text": "b" }]),
        json!([{ "type": "drag", "x": 1, "y": 2 }]),
        json!([{ "x": 1, "y": 2 }]),
    ] {
        assert!(events(bad.clone()).is_err(), "{bad}");
    }
    let args: Result<InputArgs, _> =
        serde_json::from_value(json!({ "window_id": 1, "events": [], "extra": 1 }));
    assert!(args.is_err());
    let args: Result<CatArgs, _> = serde_json::from_value(json!({ "window_id": -1 }));
    assert!(args.is_err());
}

#[test]
fn input_events_expand_clicks_scrolls_and_keys_in_order() {
    let parsed = events(json!([
        { "type": "click", "x": 10, "y": 20 },
        { "type": "scroll", "x": 1, "y": 1, "dx": 0, "dy": 30 },
        { "type": "key", "keys": "enter" },
    ]))
    .unwrap();
    let batches = input_events(&parsed, SIZE).unwrap();
    assert_eq!(
        batches.iter().map(Vec::len).collect::<Vec<_>>(),
        [3, 2, 2],
        "one batch per event"
    );
    assert_eq!(
        batches.concat(),
        [
            InputEvent::Motion { x: 10.0, y: 20.0 },
            InputEvent::Button {
                code: 0x110,
                pressed: true
            },
            InputEvent::Button {
                code: 0x110,
                pressed: false
            },
            InputEvent::Motion { x: 1.0, y: 1.0 },
            InputEvent::Axis { dx: 0.0, dy: 30.0 },
            InputEvent::Key {
                evdev: 28,
                pressed: true
            },
            InputEvent::Key {
                evdev: 28,
                pressed: false
            },
        ]
    );
    let right = events(json!([{ "type": "click", "x": 0, "y": 0, "button": "right" }])).unwrap();
    assert_eq!(
        input_events(&right, SIZE).unwrap()[0][1],
        InputEvent::Button {
            code: 0x111,
            pressed: true
        }
    );
}

#[test]
fn input_events_refuse_points_outside_the_window_and_bad_scrolls() {
    for (event, says) in [
        (
            json!({ "type": "click", "x": 1280, "y": 0 }),
            "outside the 1280x800 window",
        ),
        (json!({ "type": "move", "x": 0, "y": -1 }), "outside"),
        (
            json!({ "type": "scroll", "x": 0, "y": 0, "dx": 0, "dy": 1e9 }),
            "scroll distances",
        ),
        (
            json!({ "type": "key", "keys": "ctrl+nokey" }),
            "unknown key",
        ),
    ] {
        let parsed = events(json!([event])).unwrap();
        let err = input_events(&parsed, SIZE).unwrap_err();
        assert!(err.starts_with("events[0]: "), "{err}");
        assert!(err.contains(says), "{event}: {err}");
    }
    assert!(input_events(&[], SIZE).is_err());
}

/// Non-ASCII text is refused before anything reaches the window, and the
/// message names the known gap and the alternative.
#[test]
fn non_ascii_text_is_refused_as_invalid_params() {
    let parsed = events(json!([
        { "type": "key", "keys": "a" },
        { "type": "text", "text": "naïve" },
    ]))
    .unwrap();
    let err = input_events(&parsed, SIZE).unwrap_err();
    assert!(err.starts_with("events[1]: "), "{err}");
    assert!(err.contains("IME") && err.contains("page_open"), "{err}");
}

#[test]
fn input_expansion_is_bounded() {
    let text = "A".repeat(keys::MAX_TEXT);
    let many: Vec<Value> = (0..3)
        .map(|_| json!({ "type": "text", "text": text }))
        .collect();
    let parsed = events(Value::Array(many)).unwrap();
    let err = input_events(&parsed, SIZE).unwrap_err();
    assert!(err.contains("split the input"), "{err}");
}

#[test]
fn page_open_takes_web_urls_only() {
    for good in [
        "https://example.com",
        "http://example.com/a?b=c#d",
        "HTTPS://x.test",
    ] {
        assert!(check_url(good).is_ok(), "{good}");
    }
    for bad in [
        "file:///etc/passwd",
        "chrome://settings",
        "javascript:alert(1)",
        "data:text/html,x",
        "about:blank",
        "https://",
        "https:///etc",
        "example.com",
        "https://exa mple.com",
        "https://example.com/\n",
    ] {
        assert!(check_url(bad).is_err(), "{bad}");
    }
}

#[test]
fn page_refusals_name_every_page_including_unknown_ones() {
    use chrome_control::{PageInfo, Visibility};
    let page = |id: &str, visibility| PageInfo {
        target_id: id.into(),
        url: format!("https://{id}.test/"),
        title: id.into(),
        visibility,
    };
    let err = app_error(
        "page_cat",
        AppError::Chrome(chrome_control::Error::AmbiguousPage {
            pages: vec![
                page("a", Visibility::Visible),
                page("b", Visibility::Unknown),
            ],
        }),
    );
    assert_eq!(err.code, CONFLICT);
    assert!(err.message.starts_with("page_cat: "), "{}", err.message);
    assert!(err.message.contains("a (visible)"), "{}", err.message);
    assert!(err.message.contains("b (unknown)"), "{}", err.message);
    assert_eq!(err.data.as_ref().unwrap()["refusal"], "ambiguous_page");
    assert_eq!(
        err.data.as_ref().unwrap()["pages"][1]["visibility"],
        "unknown"
    );

    let err = app_error(
        "page_open",
        AppError::Chrome(chrome_control::Error::NoVisiblePage {
            pages: vec![page("h", Visibility::Hidden)],
        }),
    );
    assert_eq!(err.code, CONFLICT);
    assert!(err.message.contains("h (hidden)"), "{}", err.message);
    assert_eq!(err.data.as_ref().unwrap()["refusal"], "no_visible_page");
}

#[test]
fn unavailable_is_an_error_never_a_result() {
    assert_eq!(
        app_error("page_cat", AppError::NotRunning).code,
        UNAVAILABLE
    );
    assert_eq!(
        app_error("page_cat", AppError::Chrome(chrome_control::Error::Exited)).code,
        UNAVAILABLE
    );
    let gone = compositor_error("window_cat", compositor::Error::WindowGone(WindowId(4)));
    assert_eq!(gone.code, NOT_FOUND);
    assert!(gone.message.starts_with("window_cat: "));
    let stopped = compositor_error("window_cat", compositor::Error::Stopped);
    assert_eq!(stopped.code, UNAVAILABLE);
}

/// The binary serves exactly the tools the manifest exposes, and the
/// manifest is one the kernel accepts, with the annotations the design fixes.
#[test]
fn the_manifest_matches_the_binary_and_the_kernel_accepts_it() {
    let text = include_str!("../manifest.json");
    let manifest =
        plugin::manifest::Manifest::parse(text).expect("the kernel accepts the manifest");
    assert_eq!(
        manifest.http_socket.as_deref(),
        Some(crate::layout::HTTP_SOCKET)
    );
    let names: Vec<&str> = manifest
        .exposes_tools
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    assert_eq!(names, TOOLS);
    for tool in &manifest.exposes_tools {
        let a = tool.annotations.as_ref().expect("annotations");
        match tool.name.as_str() {
            "window_ls" | "window_cat" | "page_cat" => {
                assert_eq!(a["readOnlyHint"], true, "{}", tool.name);
                assert_eq!(a["destructiveHint"], false, "{}", tool.name);
                assert_eq!(a["openWorldHint"], false, "{}", tool.name);
                assert_eq!(tool.approval, None, "{}", tool.name);
            }
            "window_input" | "page_open" => {
                assert_eq!(a["readOnlyHint"], false, "{}", tool.name);
                // Both can discard what the owner had open in the shared tab.
                assert_eq!(a["destructiveHint"], true, "{}", tool.name);
                assert_eq!(a["openWorldHint"], true, "{}", tool.name);
                // The owner's enable is the grant: Codex runs these two without asking.
                assert_eq!(
                    tool.approval,
                    Some(plugin::approval::ToolApproval::Delegated),
                    "{}",
                    tool.name
                );
            }
            other => panic!("unexpected tool {other}"),
        }
    }
    let raw: Value = serde_json::from_str(text).unwrap();
    let config = &raw["config_schema"];
    assert_eq!(config["required"], json!(["chrome_binary"]));
    assert_eq!(
        config["properties"]["bwrap_binary"]["default"],
        "/usr/bin/bwrap"
    );
    assert_eq!(config["properties"]["width"]["default"], 1280);
    assert_eq!(config["properties"]["height"]["default"], 800);
    assert_eq!(config["properties"]["max_fps"]["default"], 15);
}
