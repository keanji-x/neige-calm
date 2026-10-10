//! The frame feed over a real compositor window: it waits for new frames and
//! never hands out a frame of a window that has closed (A8).
#![cfg(target_os = "linux")]

mod support;

use std::process::{Child, Command};
use std::time::Duration;

use compositor::{InputEvent, WindowId};
use desktop::feed::CompositorWindow;
use desktop::windows::Windows;
use support::{FAKE_CHROME, tmp, wait_for};
use window_stream::{FrameFeed, WindowSource};

struct Client(Child);

impl Drop for Client {
    fn drop(&mut self) {
        // The test's own child.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A compositor with the fake Chrome's window on it, as a plain client.
fn window(
    run: &std::path::Path,
    home: &std::path::Path,
) -> (compositor::Compositor, Windows, WindowId, Client) {
    let (compositor, events) = compositor::start(compositor::Config {
        run_dir: run.to_path_buf(),
        size: (320, 240),
        max_fps: 30,
    })
    .unwrap();
    let windows = Windows::follow(events);
    let client = Client(
        Command::new(FAKE_CHROME)
            .env_clear()
            .env("WAYLAND_DISPLAY", compositor.wayland_socket())
            .env("XDG_RUNTIME_DIR", run)
            .env("HOME", home)
            .env("FAKE_CHROME_NO_CDP", "1")
            .spawn()
            .unwrap(),
    );
    let pid = client.0.id();
    wait_for(Duration::from_secs(10), || {
        windows.main_window(pid).is_some()
    });
    let id = windows.main_window(pid).unwrap().id;
    (compositor, windows, id, client)
}

fn dirs() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tmp();
    let (run, home) = (tmp.path().join("run"), tmp.path().join("home"));
    std::fs::create_dir_all(&run).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    (tmp, run, home)
}

async fn next(feed: &mut Box<dyn FrameFeed>, within: Duration) -> Option<Option<u32>> {
    tokio::time::timeout(within, feed.next())
        .await
        .ok()
        .map(|frame| frame.map(|f| f.pixel_centre()))
}

trait Centre {
    fn pixel_centre(&self) -> u32;
}

impl Centre for window_stream::Frame {
    fn pixel_centre(&self) -> u32 {
        let (x, y) = (self.size.0 / 2, self.size.1 / 2);
        let at = (y * self.stride + x * 4) as usize;
        let px = &self.xrgb8888[at..at + 4];
        u32::from_le_bytes([px[0], px[1], px[2], 0])
    }
}

/// A window that closes right after the watch began: its first frame was in
/// the slot, and the feed must end without handing it out.
#[tokio::test(flavor = "multi_thread")]
async fn a_window_closed_before_the_first_read_yields_no_frame() {
    let (_tmp, run, home) = dirs();
    let (compositor, windows, id, _client) = window(&run, &home);
    let source = CompositorWindow::new(compositor, windows.clone(), id);
    let mut feed = source.watch().unwrap();
    std::fs::write(home.join("close"), b"").unwrap();
    wait_for(Duration::from_secs(5), || windows.get(id).is_none());
    assert_eq!(next(&mut feed, Duration::from_secs(5)).await, Some(None));
}

/// The feed waits for a new frame instead of repeating one, delivers the
/// window's next drawing, and ends when the window closes even though a
/// newer frame was waiting.
#[tokio::test(flavor = "multi_thread")]
async fn the_feed_waits_for_new_frames_and_ends_with_the_window() {
    let (_tmp, run, home) = dirs();
    let (compositor, windows, id, _client) = window(&run, &home);
    let source = CompositorWindow::new(compositor.clone(), windows.clone(), id);
    let mut feed = source.watch().unwrap();
    assert_eq!(
        next(&mut feed, Duration::from_secs(5)).await,
        Some(Some(support::BLUE))
    );
    assert_eq!(feed.title(), "Fake Chrome");
    // Nothing new: the feed stays pending.
    assert_eq!(next(&mut feed, Duration::from_millis(300)).await, None);
    // `r`: the client draws red at its next frame callback.
    compositor
        .input(
            id,
            vec![
                InputEvent::Key {
                    evdev: 19,
                    pressed: true,
                },
                InputEvent::Key {
                    evdev: 19,
                    pressed: false,
                },
            ],
        )
        .unwrap();
    assert_eq!(
        next(&mut feed, Duration::from_secs(5)).await,
        Some(Some(support::RED))
    );
    // Another frame waits in the slot when the window closes.
    compositor
        .input(
            id,
            vec![InputEvent::Key {
                evdev: 19,
                pressed: true,
            }],
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(home.join("close"), b"").unwrap();
    wait_for(Duration::from_secs(5), || windows.get(id).is_none());
    assert_eq!(next(&mut feed, Duration::from_secs(5)).await, Some(None));
}
