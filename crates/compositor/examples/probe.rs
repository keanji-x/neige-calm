//! Probe: run a real Chrome on the compositor without a GPU and record what happens.
//!
//! Usage: `probe <chrome-binary> [out-dir] [-- extra chrome flags...]`
//!
//! Starts the compositor, launches Chrome on a local test page with an
//! allowlisted environment, dumps PNGs of its window, clicks the `<input>`,
//! types "hello", opens the `<select>` popup and picks an option with the
//! keyboard, then measures idle and watched CPU and memory of both sides.
//! Everything lands in `out-dir` (default `$TMPDIR/probe`).
#[cfg(target_os = "linux")]
fn main() -> probe::Res<()> {
    probe::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("the probe runs on Linux only");
}

#[cfg(target_os = "linux")]
mod probe {
    use std::collections::{BTreeMap, BTreeSet};
    use std::error::Error;
    use std::fs::{self, File};
    use std::io::BufWriter;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use compositor::{
        Compositor, Config, DEFAULT_MAX_FPS, DEFAULT_SIZE, Frame, InputEvent, SOCKET_NAME,
        WindowEvent, WindowId,
    };

    pub type Res<T> = Result<T, Box<dyn Error>>;

    const MAGENTA: u32 = 0x00ff_00ff;
    const BTN_LEFT: u32 = 0x110;
    const KEY_DOWN: u32 = 108;
    const KEY_ENTER: u32 = 28;
    /// Variables Chrome inherits from this process; everything else is dropped.
    const ENV_ALLOWLIST: [&str; 3] = ["HOME", "LANG", "PATH"];
    const IDLE_SPAN: Duration = Duration::from_secs(60);
    const WATCHED_SPAN: Duration = Duration::from_secs(20);

    const PAGE: &str = r#"<!doctype html>
<html><head><title>probe</title><style>
html, body { margin: 0; height: 100%; background: #ff00ff; }
#i { position: absolute; left: 40px; top: 40px; width: 300px; height: 36px; font-size: 24px; }
#s { position: absolute; left: 40px; top: 120px; width: 300px; height: 36px; font-size: 20px; }
</style></head><body>
<input id="i" oninput="document.title = 'typed:' + this.value">
<select id="s" onchange="document.title = 'selected:' + this.value">
<option>alpha</option><option>bravo</option><option>charlie</option><option>delta</option>
</select>
</body></html>
"#;

    pub fn main() -> Res<()> {
        let mut args = std::env::args().skip(1);
        let chrome = PathBuf::from(
            args.next()
                .ok_or("usage: probe <chrome-binary> [out-dir] [-- extra chrome flags...]")?,
        );
        let tmp = std::env::temp_dir();
        let mut out = tmp.join("probe");
        let mut extra_flags = Vec::new();
        while let Some(arg) = args.next() {
            if arg == "--" {
                extra_flags.extend(args.by_ref());
            } else {
                out = PathBuf::from(arg);
            }
        }
        fs::create_dir_all(&out)?;
        let run_dir = tmp.join("probe-run");
        fs::create_dir_all(&run_dir)?;
        fs::set_permissions(&run_dir, fs::Permissions::from_mode(0o700))?;
        // Only this probe uses this run dir, so a leftover socket is from an earlier probe.
        let _ = fs::remove_file(run_dir.join(SOCKET_NAME));
        let profile = tmp.join("probe-profile");
        let _ = fs::remove_dir_all(&profile);
        fs::create_dir_all(&profile)?;

        let (compositor, events) = compositor::start(Config {
            run_dir: run_dir.clone(),
            size: DEFAULT_SIZE,
            max_fps: DEFAULT_MAX_FPS,
        })?;
        let page = out.join("page.html");
        fs::write(&page, PAGE)?;

        let log_path = out.join("chrome.log");
        let log = File::create(&log_path)?;
        let mut command = Command::new(&chrome);
        command.env_clear();
        for key in ENV_ALLOWLIST {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("WAYLAND_DISPLAY", compositor.wayland_socket())
            .env("XDG_RUNTIME_DIR", &run_dir)
            .arg("--ozone-platform=wayland")
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .args(&extra_flags)
            .arg(format!("file://{}", page.display()))
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0);
        println!("chrome flags: {:?}", command.get_args().collect::<Vec<_>>());
        let launched = Instant::now();
        let mut child = command.spawn()?;
        let chrome_pid = child.id() as i32;
        let result = drive(&compositor, &events, chrome_pid, launched, &out);
        stop(&mut child, chrome_pid);
        println!("chrome log: {}", log_path.display());
        result
    }

    fn drive(
        compositor: &Compositor,
        events: &std::sync::mpsc::Receiver<WindowEvent>,
        chrome_pid: i32,
        launched: Instant,
        out: &Path,
    ) -> Res<()> {
        let window = loop {
            let left = Duration::from_secs(30).saturating_sub(launched.elapsed());
            match events.recv_timeout(left) {
                Ok(WindowEvent::Opened(info)) if info.pid == chrome_pid => break info,
                Ok(other) => println!("event: {other:?}"),
                Err(_) => return Err("no Chrome window within 30 s (see chrome.log)".into()),
            }
        };
        println!(
            "window {} opened {:?} after launch: {:?}",
            window.id,
            launched.elapsed(),
            window
        );
        let id = window.id;

        let (loaded, origin) = poll(Duration::from_secs(30), || {
            let frame = compositor.capture(id).ok()?;
            content_origin(&frame).map(|origin| (frame, origin))
        })
        .ok_or("the page never painted its magenta background")?;
        println!(
            "page painted {:?} after launch; content origin {origin:?}",
            launched.elapsed()
        );
        save_png(&loaded, &out.join("01-loaded.png"))?;
        record_processes(chrome_pid, &out.join("processes.txt"))?;

        // Click the input and type: first unwatched (1 Hz frame callbacks), then
        // watched (max_fps), timing how long until the typed text shows in a frame.
        let text_box = ((origin.0 + 44, origin.1 + 44), (292, 28));
        click(compositor, id, (origin.0 + 190, origin.1 + 58))?;
        std::thread::sleep(Duration::from_millis(300));
        let before = compositor.capture(id)?;
        type_ascii(compositor, id, "hel")?;
        let unwatched_lag = time_until_changed(compositor, id, &before, text_box);
        println!("unwatched: typed text visible in a capture after {unwatched_lag:?}");
        let watch = compositor.watch(id)?;
        let before = compositor.capture(id)?;
        type_ascii(compositor, id, "lo")?;
        let watched_lag = time_until_changed(compositor, id, &before, text_box);
        println!("watched: typed text visible in a capture after {watched_lag:?}");
        let typed = poll(Duration::from_secs(5), || {
            title(compositor, id).filter(|t| t.contains("typed:hello"))
        });
        println!("title after typing: {typed:?}");
        let after_typing = compositor.capture(id)?;
        save_png(&after_typing, &out.join("02-typed.png"))?;
        let text_pixels = changed_pixels(&loaded, &after_typing, text_box.0, text_box.1);
        println!("pixels changed inside the input box after typing: {text_pixels}");

        // Open the select popup.
        click(compositor, id, (origin.0 + 190, origin.1 + 138))?;
        std::thread::sleep(Duration::from_millis(1000));
        let popup_open = compositor.capture(id)?;
        save_png(&popup_open, &out.join("03-select-open.png"))?;
        let below = (origin.0 + 40, origin.1 + 160);
        let popup_pixels = changed_pixels(&after_typing, &popup_open, below, (300, 120));
        println!("pixels changed below the select with the popup open: {popup_pixels} of 36000");

        // Pick the next option with the keyboard through the popup grab.
        key(compositor, id, KEY_DOWN)?;
        key(compositor, id, KEY_ENTER)?;
        let selected = poll(Duration::from_secs(5), || {
            title(compositor, id).filter(|t| t.contains("selected:"))
        });
        println!("title after choosing with the keyboard: {selected:?}");
        std::thread::sleep(Duration::from_millis(500));
        let closed = compositor.capture(id)?;
        save_png(&closed, &out.join("04-select-chosen.png"))?;

        measure("watched", chrome_pid, WATCHED_SPAN, Some(&watch))?;
        drop(watch);
        measure("idle (unwatched)", chrome_pid, IDLE_SPAN, None)?;
        let last = compositor.capture(id)?;
        save_png(&last, &out.join("05-final.png"))?;
        println!("PNGs in {}", out.display());
        Ok(())
    }

    fn title(compositor: &Compositor, id: WindowId) -> Option<String> {
        compositor
            .windows()
            .ok()?
            .into_iter()
            .find(|w| w.id == id)
            .map(|w| w.title)
    }

    fn click(compositor: &Compositor, id: WindowId, (x, y): (u32, u32)) -> Res<()> {
        let (x, y) = (f64::from(x), f64::from(y));
        compositor.input(id, vec![InputEvent::Motion { x, y }])?;
        std::thread::sleep(Duration::from_millis(50));
        compositor.input(
            id,
            vec![InputEvent::Button {
                code: BTN_LEFT,
                pressed: true,
            }],
        )?;
        std::thread::sleep(Duration::from_millis(50));
        compositor.input(
            id,
            vec![InputEvent::Button {
                code: BTN_LEFT,
                pressed: false,
            }],
        )?;
        Ok(())
    }

    fn key(compositor: &Compositor, id: WindowId, evdev: u32) -> Res<()> {
        for pressed in [true, false] {
            compositor.input(id, vec![InputEvent::Key { evdev, pressed }])?;
            std::thread::sleep(Duration::from_millis(40));
        }
        Ok(())
    }

    fn type_ascii(compositor: &Compositor, id: WindowId, text: &str) -> Res<()> {
        for c in text.chars() {
            let evdev = match c {
                'e' => 18,
                'h' => 35,
                'l' => 38,
                'o' => 24,
                other => return Err(format!("no evdev code for {other:?} in this probe").into()),
            };
            key(compositor, id, evdev)?;
        }
        Ok(())
    }

    fn poll<T>(timeout: Duration, mut attempt: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = attempt() {
                return Some(value);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// The top-left pixel of the page's magenta viewport.
    fn content_origin(frame: &Frame) -> Option<(u32, u32)> {
        (0..frame.size.1).find_map(|y| {
            (0..frame.size.0)
                .find(|&x| {
                    frame.pixel(x, y) == MAGENTA
                        && x + 8 < frame.size.0
                        && y + 8 < frame.size.1
                        && frame.pixel(x + 8, y + 8) == MAGENTA
                })
                .map(|x| (x, y))
        })
    }

    /// Polls captures until the region differs from `base`.
    fn time_until_changed(
        compositor: &Compositor,
        id: WindowId,
        base: &Frame,
        (at, size): ((u32, u32), (u32, u32)),
    ) -> Option<Duration> {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            let frame = compositor.capture(id).ok()?;
            if changed_pixels(base, &frame, at, size) > 0 {
                return Some(started.elapsed());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    fn changed_pixels(a: &Frame, b: &Frame, (x0, y0): (u32, u32), (w, h): (u32, u32)) -> usize {
        let mut changed = 0;
        for y in y0..(y0 + h).min(a.size.1).min(b.size.1) {
            for x in x0..(x0 + w).min(a.size.0).min(b.size.0) {
                if a.pixel(x, y) != b.pixel(x, y) {
                    changed += 1;
                }
            }
        }
        changed
    }

    fn save_png(frame: &Frame, path: &Path) -> Res<()> {
        let mut rgb = Vec::with_capacity((frame.size.0 * frame.size.1 * 3) as usize);
        for y in 0..frame.size.1 {
            for x in 0..frame.size.0 {
                let p = frame.pixel(x, y);
                rgb.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
            }
        }
        let mut encoder = png::Encoder::new(
            BufWriter::new(File::create(path)?),
            frame.size.0,
            frame.size.1,
        );
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&rgb)?;
        println!("wrote {}", path.display());
        Ok(())
    }

    /// Chrome's processes: its process group plus every descendant of the browser.
    fn chrome_processes(chrome_pid: i32) -> BTreeSet<i32> {
        let mut parent = BTreeMap::new();
        let mut found = BTreeSet::new();
        for (pid, stat) in proc_stats() {
            let fields: Vec<&str> = stat.split_whitespace().collect();
            let (ppid, pgid) = (
                fields[1].parse().unwrap_or(0),
                fields[2].parse().unwrap_or(0),
            );
            parent.insert(pid, ppid);
            if pgid == chrome_pid {
                found.insert(pid);
            }
        }
        found.insert(chrome_pid);
        loop {
            let more: Vec<i32> = parent
                .iter()
                .filter(|(pid, ppid)| found.contains(ppid) && !found.contains(pid))
                .map(|(pid, _)| *pid)
                .collect();
            if more.is_empty() {
                return found;
            }
            found.extend(more);
        }
    }

    /// `(pid, stat fields after the command name)` for every process.
    fn proc_stats() -> Vec<(i32, String)> {
        let Ok(entries) = fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| {
                let pid: i32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
                let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                let rest = stat.rsplit_once(')')?.1.to_string();
                Some((pid, rest))
            })
            .collect()
    }

    /// utime + stime in clock ticks.
    fn cpu_ticks(pid: i32) -> u64 {
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return 0;
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            return 0;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // After ")": state(0) ppid(1) ... utime(11) stime(12).
        fields[11].parse::<u64>().unwrap_or(0) + fields[12].parse::<u64>().unwrap_or(0)
    }

    /// (RSS, PSS) in KiB.
    fn memory_kib(pid: i32) -> (u64, u64) {
        let read = |file: &str, key: &str| {
            fs::read_to_string(format!("/proc/{pid}/{file}"))
                .ok()
                .and_then(|text| {
                    text.lines()
                        .find(|l| l.starts_with(key))
                        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
                })
                .unwrap_or(0)
        };
        (read("status", "VmRSS:"), read("smaps_rollup", "Pss:"))
    }

    fn clock_ticks_per_second() -> f64 {
        Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok()?.trim().parse().ok())
            .unwrap_or(100.0)
    }

    fn measure(
        label: &str,
        chrome_pid: i32,
        span: Duration,
        watch: Option<&compositor::FrameWatch>,
    ) -> Res<()> {
        let me = std::process::id() as i32;
        let chrome_before: BTreeMap<i32, u64> = chrome_processes(chrome_pid)
            .into_iter()
            .map(|pid| (pid, cpu_ticks(pid)))
            .collect();
        let me_before = cpu_ticks(me);
        let started = Instant::now();
        let mut frames = 0u32;
        while started.elapsed() < span {
            match watch {
                Some(watch) => {
                    if watch.take_timeout(Duration::from_millis(200))?.is_some() {
                        frames += 1;
                    }
                }
                None => std::thread::sleep(Duration::from_millis(200)),
            }
        }
        let seconds = started.elapsed().as_secs_f64();
        let hz = clock_ticks_per_second();
        let me_cpu = (cpu_ticks(me) - me_before) as f64 / hz / seconds * 100.0;
        let chrome_now = chrome_processes(chrome_pid);
        let chrome_cpu: f64 = chrome_now
            .iter()
            .map(|pid| {
                cpu_ticks(*pid).saturating_sub(chrome_before.get(pid).copied().unwrap_or(0)) as f64
            })
            .sum::<f64>()
            / hz
            / seconds
            * 100.0;
        let (me_rss, me_pss) = memory_kib(me);
        let (chrome_rss, chrome_pss) = chrome_now
            .iter()
            .map(|pid| memory_kib(*pid))
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        println!(
            "{label} over {seconds:.0} s: compositor(probe process) cpu {me_cpu:.2}% rss {} MiB pss {} MiB; \
             chrome ({} processes) cpu {chrome_cpu:.2}% rss-sum {} MiB pss-sum {} MiB; frames taken {frames}",
            me_rss / 1024,
            me_pss / 1024,
            chrome_now.len(),
            chrome_rss / 1024,
            chrome_pss / 1024,
        );
        Ok(())
    }

    fn record_processes(chrome_pid: i32, path: &Path) -> Res<()> {
        let mut lines = Vec::new();
        for pid in chrome_processes(chrome_pid) {
            let cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
            lines.push(format!("{pid} {cmdline}"));
        }
        fs::write(path, lines.join("\n") + "\n")?;
        println!("process list: {}", path.display());
        Ok(())
    }

    fn stop(child: &mut Child, chrome_pid: i32) {
        for (signal, wait) in [("-TERM", Duration::from_secs(2)), ("-KILL", Duration::ZERO)] {
            let _ = Command::new("kill")
                .args([signal, "--", &format!("-{chrome_pid}")])
                .status();
            std::thread::sleep(wait);
        }
        let _ = child.wait();
    }
}
