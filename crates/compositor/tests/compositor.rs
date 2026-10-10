//! Compositor behaviour through its public handle, driven by real Wayland
//! clients over the compositor's socket.
#![cfg(target_os = "linux")]

mod support;

use std::time::Duration;

use compositor::{Error, Frame, InputEvent, WindowEvent};
use support::{BTN_LEFT, KEY_A, Seen, TestClient, run_dir, start, wait_opened};

const RED: u32 = 0x00d0_2020;
const BLUE: u32 = 0x0020_20d0;
const GREEN: u32 = 0x0020_c020;
const SIZE: (u32, u32) = (160, 120);

/// Every pixel of `frame`, as `0x00RRGGBB`.
fn colours(frame: &Frame) -> std::collections::BTreeMap<u32, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for y in 0..frame.size.1 {
        for x in 0..frame.size.0 {
            *counts.entry(frame.pixel(x, y)).or_default() += 1;
        }
    }
    counts
}

#[test]
fn each_frame_holds_only_its_own_window_and_popups() {
    let dir = run_dir();
    let (compositor, events) = start(dir.path(), SIZE, 30);
    let mut red = TestClient::connect(compositor.wayland_socket());
    red.open_window("red", RED);
    let red_info = wait_opened(&events);
    let mut blue = TestClient::connect(compositor.wayland_socket());
    blue.open_window("blue", BLUE);
    let blue_info = wait_opened(&events);

    assert_ne!(red_info.id, blue_info.id);
    assert_eq!(
        red_info.size, SIZE,
        "toplevels are configured to the output size"
    );
    assert_eq!(red_info.title, "red");
    assert_eq!(red_info.pid, std::process::id() as i32);
    assert_eq!(
        compositor.windows().unwrap(),
        vec![red_info.clone(), blue_info.clone()]
    );

    // A popup that asks to extend past the window is constrained into it.
    red.open_popup(GREEN, (140, 100, 40, 30));
    red.roundtrip();

    let red_frame = compositor.capture(red_info.id).unwrap();
    let blue_frame = compositor.capture(blue_info.id).unwrap();
    assert_eq!(red_frame.size, SIZE);
    assert_eq!(red_frame.stride, SIZE.0 * 4);
    let red_colours = colours(&red_frame);
    assert_eq!(
        red_colours.keys().copied().collect::<Vec<_>>(),
        vec![GREEN, RED],
        "red's frame holds red and its popup, nothing else: {red_colours:x?}"
    );
    assert_eq!(
        red_colours[&GREEN],
        40 * 30,
        "the whole popup is inside the frame"
    );
    assert_eq!(
        colours(&blue_frame).into_iter().collect::<Vec<_>>(),
        vec![(BLUE, (SIZE.0 * SIZE.1) as usize)],
        "blue's frame holds only blue"
    );
}

#[test]
fn a_closed_or_emptied_window_errors_instead_of_returning_a_stale_frame() {
    let dir = run_dir();
    let (compositor, events) = start(dir.path(), SIZE, 30);
    let mut kept = TestClient::connect(compositor.wayland_socket());
    kept.open_window("kept", BLUE);
    let kept_info = wait_opened(&events);
    let mut closing = TestClient::connect(compositor.wayland_socket());
    closing.open_window("closing", RED);
    let info = wait_opened(&events);

    compositor.capture(info.id).expect("capture while open");
    let watch = compositor.watch(info.id).expect("watch while open");
    closing.disconnect();
    let closed = events
        .recv_timeout(Duration::from_secs(5))
        .expect("close event");
    assert_eq!(closed, WindowEvent::Closed(info.id));

    assert!(matches!(compositor.capture(info.id), Err(Error::WindowGone(id)) if id == info.id));
    assert!(matches!(watch.try_take(), Err(Error::WindowGone(_))));
    assert!(matches!(
        compositor.input(
            info.id,
            vec![InputEvent::Key {
                evdev: KEY_A,
                pressed: true
            }]
        ),
        Err(Error::WindowGone(_))
    ));
    assert_eq!(compositor.windows().unwrap(), vec![kept_info.clone()]);

    // A window whose buffer is detached has no content to show.
    kept.unmap();
    assert!(matches!(
        compositor.capture(kept_info.id),
        Err(Error::NoBuffer(_))
    ));
}

#[test]
fn input_reaches_the_targeted_window_and_moves_keyboard_focus() {
    let dir = run_dir();
    let (compositor, events) = start(dir.path(), SIZE, 30);
    let mut first = TestClient::connect(compositor.wayland_socket());
    first.open_window("first", RED);
    let first_info = wait_opened(&events);
    let mut second = TestClient::connect(compositor.wayland_socket());
    second.open_window("second", BLUE);
    let second_info = wait_opened(&events);

    compositor
        .input(
            second_info.id,
            vec![
                InputEvent::Motion { x: 10.0, y: 20.0 },
                InputEvent::Button {
                    code: BTN_LEFT,
                    pressed: true,
                },
                InputEvent::Button {
                    code: BTN_LEFT,
                    pressed: false,
                },
                InputEvent::Key {
                    evdev: KEY_A,
                    pressed: true,
                },
                InputEvent::Key {
                    evdev: KEY_A,
                    pressed: false,
                },
            ],
        )
        .unwrap();
    second.roundtrip();
    first.roundtrip();
    let seen = std::mem::take(&mut second.state.seen);
    assert!(seen.contains(&Seen::KeyboardEnter), "{seen:?}");
    assert!(
        seen.iter().any(|s| matches!(s, Seen::PointerMotion { x, y } | Seen::PointerEnter { x, y } if *x == 10.0 && *y == 20.0)),
        "{seen:?}"
    );
    let buttons_and_keys: Vec<_> = seen
        .into_iter()
        .filter(|s| matches!(s, Seen::Button { .. } | Seen::Key { .. }))
        .collect();
    assert_eq!(
        buttons_and_keys,
        vec![
            Seen::Button {
                button: BTN_LEFT,
                pressed: true
            },
            Seen::Button {
                button: BTN_LEFT,
                pressed: false
            },
            Seen::Key {
                key: KEY_A,
                pressed: true
            },
            Seen::Key {
                key: KEY_A,
                pressed: false
            },
        ]
    );
    assert!(
        !first
            .state
            .seen
            .iter()
            .any(|s| matches!(s, Seen::Key { .. } | Seen::Button { .. })),
        "the other window got nothing: {:?}",
        first.state.seen
    );

    compositor
        .input(
            first_info.id,
            vec![
                InputEvent::Key {
                    evdev: KEY_A,
                    pressed: true,
                },
                InputEvent::Key {
                    evdev: KEY_A,
                    pressed: false,
                },
            ],
        )
        .unwrap();
    first.roundtrip();
    second.roundtrip();
    assert!(first.state.seen.contains(&Seen::KeyboardEnter));
    assert!(first.state.seen.contains(&Seen::Key {
        key: KEY_A,
        pressed: true
    }));
    assert_eq!(
        second.state.seen,
        vec![Seen::KeyboardLeave],
        "focus left the second window"
    );
}

#[test]
fn watched_windows_get_fast_frame_callbacks_and_unwatched_ones_one_per_second() {
    const MAX_FPS: u32 = 20;
    const SPAN: Duration = Duration::from_secs(2);
    let dir = run_dir();
    let (compositor, events) = start(dir.path(), SIZE, MAX_FPS);
    let mut client = TestClient::connect(compositor.wayland_socket());
    client.open_window("paced", RED);
    let info = wait_opened(&events);
    client.start_frame_loop();

    let count_over = |client: &mut TestClient| {
        let before = client.state.frame_callbacks;
        client.pump(SPAN);
        client.state.frame_callbacks - before
    };
    let idle = count_over(&mut client);
    assert!(
        (1..=3).contains(&idle),
        "unwatched: {idle} callbacks in 2 s"
    );

    let watch = compositor.watch(info.id).unwrap();
    let first = watch
        .try_take()
        .unwrap()
        .expect("a watch starts with a full frame");
    assert_eq!(first.damage.len(), 1);
    assert_eq!((first.damage[0].width, first.damage[0].height), SIZE);
    let watched = count_over(&mut client);
    assert!(
        (2 * MAX_FPS - 12..=2 * MAX_FPS + 2).contains(&watched),
        "watched: {watched} callbacks in 2 s"
    );

    // New content reaches the watcher with its damage.
    client.draw(BLUE, (SIZE.0 as i32, SIZE.1 as i32));
    let frame = watch
        .take_timeout(Duration::from_secs(2))
        .unwrap()
        .expect("a frame after the redraw");
    assert_eq!(frame.pixel(5, 5), BLUE);
    assert!(!frame.damage.is_empty());

    drop(watch);
    client.pump(Duration::from_millis(200));
    let after = count_over(&mut client);
    assert!(
        (1..=3).contains(&after),
        "after unwatch: {after} callbacks in 2 s"
    );
}
