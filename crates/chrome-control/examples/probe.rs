//! Probe: drive a real Chrome through `chrome-control` on the `compositor`
//! crate's Wayland display and record what happens.
//!
//! Usage: `probe <chrome-binary> [out-dir]` (default out-dir `$TMPDIR/probe`).
//!
//! Records, in order:
//! - (a) launch, `navigate` to `data:`, `file:` and https pages, `read_page`;
//! - (b) `navigator.webdriver` under `--remote-debugging-pipe`;
//! - (f) two visible windows after a page opens a popup: `AmbiguousPage`;
//! - (c) the launch's process set (descendants by parent links, the crashpad
//!   handlers named by `--crashpad-handler-pid`, and every crashpad handler
//!   on the same private crash database, i.e. the `--monitor-self` peer) and
//!   which of them survive 5 s after `stop()` and after SIGKILL of an owner
//!   process; measured only, never signalled;
//! - (d) a relaunch on the same profile right after the owner's SIGKILL;
//! - (f, alert) a cross-site popup held up by `alert()` beside its visible
//!   opener: `AmbiguousPage` (served by a loopback HTTP server in the probe);
//! - (e) a second launch while the first holds the profile: `ProfileBusy`.
//!
//! `probe owner <chrome> <profile> <home> <display> <runtime-dir>` is the
//! internal owner process: it launches Chrome, prints `browser <pid>` and
//! holds it until it is killed or its stdin closes.
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
    use std::fs;
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use chrome_control::{Chrome, LaunchConfig, WaylandEnv};
    use compositor::{Compositor, Config, DEFAULT_MAX_FPS, DEFAULT_SIZE, InputEvent, SOCKET_NAME};

    pub type Res<T> = Result<T, Box<dyn Error>>;

    const NAV_TIMEOUT: Duration = Duration::from_secs(20);
    const SURVIVAL_WAIT: Duration = Duration::from_secs(5);
    const BTN_LEFT: u32 = 0x110;

    const FILE_PAGE: &str = "<!doctype html><title>file page</title><body><h1>Hello from a file</h1>\
        <p>chrome-control probe</p></body>";
    const WEBDRIVER_PAGE: &str = "data:text/html,<title>webdriver</title><body><script>\
        document.body.innerText = 'webdriver=' + navigator.webdriver;</script></body>";
    const POPUP_PAGE: &str = "data:text/html,<title>opener</title><body style='margin:0'>\
        <button style='width:100vw;height:100vh;font-size:40px' onclick=\"window.open(\
        'data:text/html,<title>popup</title>popup body', 'pop', 'popup,width=400,height=300')\">\
        open popup</button></body>";

    pub fn main() -> Res<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("owner") {
            return owner(&args[1..]);
        }
        let chrome = PathBuf::from(
            args.first()
                .ok_or("usage: probe <chrome-binary> [out-dir]")?,
        );
        let out = args
            .get(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("probe"));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        runtime.block_on(run(chrome, out))
    }

    struct Setup {
        chrome: PathBuf,
        out: PathBuf,
        profile: PathBuf,
        home: PathBuf,
        compositor: Compositor,
        run_dir: PathBuf,
    }

    impl Setup {
        fn config(&self) -> LaunchConfig {
            LaunchConfig {
                binary: self.chrome.clone(),
                profile_dir: self.profile.clone(),
                home_dir: self.home.clone(),
                wayland: WaylandEnv {
                    display: self.compositor.wayland_socket().into(),
                    runtime_dir: self.run_dir.clone(),
                },
                size: DEFAULT_SIZE,
            }
        }
    }

    async fn run(chrome: PathBuf, out: PathBuf) -> Res<()> {
        // Only this probe uses `out`, so anything in it is from an earlier probe run.
        let _ = fs::remove_dir_all(&out);
        fs::create_dir_all(&out)?;
        let run_dir = out.join("run");
        fs::create_dir_all(&run_dir)?;
        fs::set_permissions(&run_dir, fs::Permissions::from_mode(0o700))?;
        let (compositor, _events) = compositor::start(Config {
            run_dir: run_dir.clone(),
            size: DEFAULT_SIZE,
            max_fps: DEFAULT_MAX_FPS,
        })?;
        println!("socket: {}", run_dir.join(SOCKET_NAME).display());
        let version = Command::new(&chrome).arg("--version").output()?;
        println!(
            "chrome: {} -> {}",
            chrome.display(),
            String::from_utf8_lossy(&version.stdout).trim()
        );
        let setup = Setup {
            chrome,
            profile: out.join("profile"),
            home: out.join("home"),
            out,
            compositor,
            run_dir,
        };

        println!("\n== (a) launch, navigate, read_page; (b) navigator.webdriver");
        let started = Instant::now();
        let first = Chrome::launch(setup.config()).await?;
        println!("launched pid {} in {:?}", first.pid(), started.elapsed());
        println!("pages at start: {:#?}", first.pages().await?);
        let page = setup.out.join("page.html");
        fs::write(&page, FILE_PAGE)?;
        for url in [
            WEBDRIVER_PAGE.to_owned(),
            format!("file://{}", page.display()),
            "https://example.com/".to_owned(),
        ] {
            let started = Instant::now();
            match first.navigate(&url, NAV_TIMEOUT).await {
                Ok(navigated) => println!(
                    "navigate {:.60} -> {navigated:?} in {:?}",
                    url,
                    started.elapsed()
                ),
                Err(error) => println!("navigate {:.60} -> ERROR {error}", url),
            }
            match first.read_page().await {
                Ok(text) => println!("read_page -> {text:?}"),
                Err(error) => println!("read_page -> ERROR {error}"),
            }
        }

        let shot = setup.out.join("window.png");
        save_window_png(&setup.compositor, first.pid() as i32, &shot).await?;
        println!(
            "window PNG (check for an unsupported-flag infobar): {}",
            shot.display()
        );

        println!("\n== (f) two visible windows");
        two_windows(&setup, &first).await?;

        println!("\n== (c) stop(): process set and survivors");
        let before = launch_set(first.pid() as i32);
        print_set(&before);
        let started = Instant::now();
        let status = first.stop().await?;
        println!("stop() -> {status:?} in {:?}", started.elapsed());
        survivors(&before, "stop()", started).await;

        println!("\n== (c) SIGKILL of the owner process: process set and survivors");
        let (mut owner, browser) = start_owner(&setup)?;
        // Let the launch settle (helpers, crashpad) before recording it.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let before = launch_set(browser);
        print_set(&before);
        owner.kill()?; // SIGKILL our own child
        owner.wait()?;
        let killed = Instant::now();

        println!("\n== (d) relaunch on the same profile right after the SIGKILL");
        let (second, attempts) = relaunch(&setup).await?;
        println!(
            "relaunched pid {} {:?} after the SIGKILL, attempts: {attempts:?}",
            second.pid(),
            killed.elapsed()
        );
        survivors(&before, "owner SIGKILL", killed).await;
        match second
            .navigate(&format!("file://{}", page.display()), NAV_TIMEOUT)
            .await
        {
            Ok(navigated) => println!("relaunched navigate -> {navigated:?}"),
            Err(error) => println!("relaunched navigate -> ERROR {error}"),
        }

        println!("\n== (f, alert) a cross-site popup that shows alert()");
        alert_popup(&setup, &second).await?;

        println!("\n== (e) restart overlap: a second launch while the first holds the profile");
        let windows_before = setup.compositor.windows()?.len();
        let started = Instant::now();
        match Chrome::launch(setup.config()).await {
            Ok(third) => println!("UNEXPECTED: second launch succeeded, pid {}", third.pid()),
            Err(error) => println!("second launch -> {error:?} in {:?}", started.elapsed()),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        println!(
            "holder windows before/after the hand-off: {windows_before}/{}",
            setup.compositor.windows()?.len()
        );

        println!("\n== (c) stop() again on the relaunched browser");
        let before = launch_set(second.pid() as i32);
        print_set(&before);
        let started = Instant::now();
        let status = second.stop().await?;
        println!("stop() -> {status:?} in {:?}", started.elapsed());
        survivors(&before, "stop() after relaunch", started).await;
        Ok(())
    }

    async fn two_windows(setup: &Setup, chrome: &Chrome) -> Res<()> {
        let navigated = chrome.navigate(POPUP_PAGE, NAV_TIMEOUT).await?;
        println!("opener: {navigated:?}");
        click_for_a_new_window(setup, chrome).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        println!("compositor windows: {:#?}", setup.compositor.windows()?);
        println!("pages: {:#?}", chrome.pages().await?);
        println!(
            "navigate -> {:?}",
            chrome.navigate("about:blank", NAV_TIMEOUT).await
        );
        println!("read_page -> {:?}", chrome.read_page().await);
        Ok(())
    }

    /// Captures the browser's window through the compositor and writes it as PNG.
    /// Watches the window for a few seconds first, so Chrome paints at full
    /// rate and the capture shows the current page rather than an early frame.
    async fn save_window_png(compositor: &Compositor, pid: i32, path: &std::path::Path) -> Res<()> {
        let window = compositor
            .windows()?
            .into_iter()
            .find(|w| w.pid == pid)
            .ok_or("no Chrome window on the compositor")?;
        let watch = compositor.watch(window.id)?;
        tokio::time::sleep(Duration::from_secs(3)).await;
        drop(watch);
        let frame = compositor.capture(window.id)?;
        let mut rgb = Vec::with_capacity((frame.size.0 * frame.size.1 * 3) as usize);
        for y in 0..frame.size.1 {
            for x in 0..frame.size.0 {
                let p = frame.pixel(x, y);
                rgb.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
            }
        }
        let file = std::io::BufWriter::new(fs::File::create(path)?);
        let mut encoder = png::Encoder::new(file, frame.size.0, frame.size.1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&rgb)?;
        Ok(())
    }

    /// Clicks the middle of the browser's newest window and waits until one
    /// more window of the browser is open.
    async fn click_for_a_new_window(setup: &Setup, chrome: &Chrome) -> Res<()> {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let pid = chrome.pid() as i32;
        let mine = || -> Res<Vec<compositor::WindowInfo>> {
            let windows = setup.compositor.windows()?;
            Ok(windows.into_iter().filter(|w| w.pid == pid).collect())
        };
        let before = mine()?;
        let window = before.last().ok_or("no Chrome window on the compositor")?;
        let (x, y) = (
            f64::from(window.size.0) / 2.0,
            f64::from(window.size.1) / 2.0,
        );
        setup
            .compositor
            .input(window.id, vec![InputEvent::Motion { x, y }])?;
        for pressed in [true, false] {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let button = InputEvent::Button {
                code: BTN_LEFT,
                pressed,
            };
            setup.compositor.input(window.id, vec![button])?;
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while mine()?.len() <= before.len() {
            if Instant::now() > deadline {
                return Err("the popup window never opened".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }

    /// (f, alert) A cross-site popup that shows `alert()`: its renderer is held
    /// up, so its visibility is unknown and the opener alone is not known to be
    /// the visible page. The opener is served from `localhost` and the popup
    /// from `127.0.0.1`, so they run in different renderer processes.
    async fn alert_popup(setup: &Setup, chrome: &Chrome) -> Res<()> {
        let port = serve_alert_pages()?;
        let opener = format!("http://localhost:{port}/opener.html");
        println!("opener: {:?}", chrome.navigate(&opener, NAV_TIMEOUT).await?);
        click_for_a_new_window(setup, chrome).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let started = Instant::now();
        println!(
            "pages: {:#?} in {:?}",
            chrome.pages().await?,
            started.elapsed()
        );
        let started = Instant::now();
        println!(
            "read_page -> {:?} in {:?}",
            chrome.read_page().await,
            started.elapsed()
        );
        let started = Instant::now();
        let navigated = chrome.navigate("about:blank", NAV_TIMEOUT).await;
        println!("navigate -> {navigated:?} in {:?}", started.elapsed());
        Ok(())
    }

    /// A loopback HTTP server for [`alert_popup`]; returns its port.
    fn serve_alert_pages() -> Res<u16> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = answer(stream, port);
            }
        });
        Ok(port)
    }

    fn answer(mut stream: std::net::TcpStream, port: u16) -> Res<()> {
        use std::io::{Read, Write};
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Ok(());
            }
            request.extend_from_slice(&chunk[..read]);
        }
        let request = String::from_utf8_lossy(&request);
        let path = request.split_whitespace().nth(1).unwrap_or("/");
        let body = match path {
            "/opener.html" => format!(
                "<!doctype html><title>alert opener</title><body style='margin:0'>\
                 <button style='width:100vw;height:100vh;font-size:40px' onclick=\"window.open(\
                 'http://127.0.0.1:{port}/alert.html', 'alertpop', 'popup,width=400,height=300')\">\
                 open alert popup</button></body>"
            ),
            "/alert.html" => "<!doctype html><title>alert popup</title><body>popup\
                <script>alert('hello from the popup')</script></body>"
                .to_owned(),
            _ => String::new(),
        };
        let status = if body.is_empty() {
            "404 Not Found"
        } else {
            "200 OK"
        };
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )?;
        Ok(())
    }

    /// Relaunch with a bounded retry on `ProfileBusy`, as the owner would.
    async fn relaunch(setup: &Setup) -> Res<(Chrome, Vec<String>)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut attempts = Vec::new();
        loop {
            match Chrome::launch(setup.config()).await {
                Ok(chrome) => {
                    attempts.push("ok".into());
                    return Ok((chrome, attempts));
                }
                Err(error @ chrome_control::Error::ProfileBusy { .. })
                    if Instant::now() < deadline =>
                {
                    attempts.push(error.to_string());
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn start_owner(setup: &Setup) -> Res<(Child, i32)> {
        let mut owner = Command::new(std::env::current_exe()?)
            .arg("owner")
            .arg(&setup.chrome)
            .arg(&setup.profile)
            .arg(&setup.home)
            .arg(setup.compositor.wayland_socket())
            .arg(&setup.run_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut line = String::new();
        BufReader::new(owner.stdout.as_mut().ok_or("owner stdout")?).read_line(&mut line)?;
        let pid = line
            .trim()
            .strip_prefix("browser ")
            .ok_or_else(|| format!("owner said {line:?}"))?
            .parse()?;
        println!("owner {} launched browser {pid}", owner.id());
        Ok((owner, pid))
    }

    fn owner(args: &[String]) -> Res<()> {
        let [chrome, profile, home, display, runtime_dir] = args else {
            return Err(
                "usage: probe owner <chrome> <profile> <home> <display> <runtime-dir>".into(),
            );
        };
        let config = LaunchConfig {
            binary: chrome.into(),
            profile_dir: profile.into(),
            home_dir: home.into(),
            wayland: WaylandEnv {
                display: display.into(),
                runtime_dir: runtime_dir.into(),
            },
            size: DEFAULT_SIZE,
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let chrome = runtime.block_on(Chrome::launch(config))?;
        println!("browser {}", chrome.pid());
        // Hold the browser until the probe kills us or closes our stdin.
        for _ in std::io::stdin().lines() {}
        drop(chrome);
        Ok(())
    }

    #[derive(Debug, Clone)]
    struct ProcRec {
        pid: i32,
        ppid: i32,
        pgid: i32,
        sid: i32,
        start: u64,
        state: char,
        cmdline: Vec<String>,
    }

    impl ProcRec {
        fn role(&self) -> String {
            let exe = self.cmdline.first().map(String::as_str).unwrap_or("");
            if exe
                .split(' ')
                .next()
                .is_some_and(|exe| exe.ends_with("chrome_crashpad_handler"))
            {
                let kind = if self.cmdline.iter().any(|a| a == "--monitor-self") {
                    "handler(--monitor-self)"
                } else {
                    "handler(monitor)"
                };
                return format!("crashpad {kind}");
            }
            self.flag("--type=")
                .map_or_else(|| "browser".into(), str::to_owned)
        }

        /// The value of `name` (`--flag=`). Chrome's child processes rewrite their
        /// command line into one space-separated string, so arguments that hold
        /// spaces are searched token by token as well.
        fn flag(&self, name: &str) -> Option<&str> {
            self.cmdline
                .iter()
                .find_map(|a| a.strip_prefix(name))
                .or_else(|| {
                    self.cmdline
                        .iter()
                        .flat_map(|a| a.split(' '))
                        .find_map(|token| token.strip_prefix(name))
                })
        }
    }

    fn all_processes() -> BTreeMap<i32, ProcRec> {
        let mut all = BTreeMap::new();
        let Ok(entries) = fs::read_dir("/proc") else {
            return all;
        };
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<i32>().ok())
            else {
                continue;
            };
            if let Some(rec) = read_proc(pid) {
                all.insert(pid, rec);
            }
        }
        all
    }

    fn read_proc(pid: i32) -> Option<ProcRec> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        let cmdline = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(ProcRec {
            pid,
            state: fields[0].chars().next()?,
            ppid: fields[1].parse().ok()?,
            pgid: fields[2].parse().ok()?,
            sid: fields[3].parse().ok()?,
            start: fields[19].parse().ok()?,
            cmdline: cmdline
                .split(|&b| b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect(),
        })
    }

    /// The launch's process set: the browser and its descendants by parent
    /// links, the crashpad handlers they name, and every crashpad handler on
    /// the same crash database (the `--monitor-self` peer).
    fn launch_set(browser: i32) -> Vec<ProcRec> {
        let all = all_processes();
        let mut set = BTreeSet::from([browser]);
        loop {
            let more: Vec<i32> = all
                .values()
                .filter(|p| set.contains(&p.ppid) && !set.contains(&p.pid))
                .map(|p| p.pid)
                .collect();
            if more.is_empty() {
                break;
            }
            set.extend(more);
        }
        let named: BTreeSet<i32> = set
            .iter()
            .filter_map(|pid| all.get(pid)?.flag("--crashpad-handler-pid=")?.parse().ok())
            .collect();
        let databases: BTreeSet<String> = named
            .iter()
            .filter_map(|pid| Some(all.get(pid)?.flag("--database=")?.to_owned()))
            .collect();
        set.extend(named);
        set.extend(
            all.values()
                .filter(|p| {
                    p.cmdline
                        .first()
                        .and_then(|exe| exe.split(' ').next())
                        .is_some_and(|exe| exe.ends_with("chrome_crashpad_handler"))
                        && p.flag("--database=")
                            .is_some_and(|db| databases.contains(db))
                })
                .map(|p| p.pid),
        );
        set.iter().filter_map(|pid| all.get(pid).cloned()).collect()
    }

    fn print_set(set: &[ProcRec]) {
        println!(
            "{:>8} {:>8} {:>8} {:>8}  role",
            "pid", "ppid", "pgid", "sid"
        );
        for p in set {
            println!(
                "{:>8} {:>8} {:>8} {:>8}  {}",
                p.pid,
                p.ppid,
                p.pgid,
                p.sid,
                p.role()
            );
        }
        for handler in set.iter().filter(|p| p.role().starts_with("crashpad")) {
            println!("{} argv: {:?}", handler.pid, handler.cmdline);
        }
    }

    /// Lists which processes of `set` still run as the same process (pid and
    /// start time, zombies excluded) [`SURVIVAL_WAIT`] after `since`; returns
    /// early once none does.
    async fn survivors(set: &[ProcRec], label: &str, since: Instant) {
        let deadline = since + SURVIVAL_WAIT;
        let alive = |p: &ProcRec| {
            read_proc(p.pid)
                .is_some_and(|now| now.start == p.start && now.state != 'Z' && now.state != 'X')
        };
        while Instant::now() < deadline && set.iter().any(alive) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let left: Vec<&ProcRec> = set.iter().filter(|p| alive(p)).collect();
        println!(
            "survivors {:?} after {label} (checked at +{:?}): {} of {} {:?}",
            SURVIVAL_WAIT,
            since.elapsed(),
            left.len(),
            set.len(),
            left.iter().map(|p| (p.pid, p.role())).collect::<Vec<_>>()
        );
    }
}
