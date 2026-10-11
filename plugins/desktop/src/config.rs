//! What the kernel hands the plugin: its identity at spawn (`NEIGE_PLUGIN_*`)
//! and its configuration in the `initialize` handshake
//! (`_meta["dev.neige/config"].values`, already merged with the manifest's
//! defaults; see `docs/architecture/plugin-handshake-meta.md`).

use std::path::PathBuf;

use serde_json::{Map, Value};

/// The one app this version runs.
pub const CHROME: &str = "chrome";

/// The plugin's identity, set by the kernel when it spawns the process.
#[derive(Debug, Clone)]
pub struct PluginEnv {
    /// The installed plugin id; it names the plugin's routes.
    pub id: String,
    /// `<plugins_data_dir>/<id>/`, the plugin's working directory.
    pub data_dir: PathBuf,
    /// The per-process token the handshake echoes back.
    pub token: String,
}

impl PluginEnv {
    /// Reads `NEIGE_PLUGIN_ID`, `NEIGE_PLUGIN_DATA_DIR` and `NEIGE_PLUGIN_TOKEN`.
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("{name} is not set; the plugin host sets it at spawn"))
        };
        Ok(Self {
            id: var("NEIGE_PLUGIN_ID")?,
            data_dir: PathBuf::from(var("NEIGE_PLUGIN_DATA_DIR")?),
            token: var("NEIGE_PLUGIN_TOKEN")?,
        })
    }

    /// The Report `window` block `src` of `app`'s main window: the kernel's
    /// plugin WebSocket route for this plugin, then this plugin's own path.
    pub fn stream_src(&self, app: &str) -> String {
        format!("/api/plugins/{}/ws/{}", self.id, stream_path(app))
    }
}

/// The path this plugin serves `app`'s main window on, on its `http_socket`.
pub fn stream_path(app: &str) -> String {
    format!("apps/{app}/stream")
}

/// The plugin's configuration, from the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopConfig {
    pub chrome_binary: PathBuf,
    /// bubblewrap, which Chrome always runs inside.
    pub bwrap_binary: PathBuf,
    /// Output size; every window is maximized to it.
    pub size: (u32, u32),
    pub max_fps: u32,
    /// Chrome's `--proxy-server`; `None` connects directly.
    pub proxy_server: Option<String>,
}

const WIDTH: (u64, u64) = (320, 3840);
const HEIGHT: (u64, u64) = (240, 2160);
const MAX_FPS: (u64, u64) = (1, 60);

impl DesktopConfig {
    /// Parses `dev.neige/config.values`. Every key the manifest gives a
    /// default must be present: the kernel has already merged the defaults,
    /// so a missing one is a manifest or kernel defect, not something to
    /// default here. `proxy_server` has no default and may be absent.
    pub fn from_values(values: &Map<String, Value>) -> Result<Self, String> {
        let chrome_binary = absolute_path(values, "chrome_binary")?;
        let bwrap_binary = absolute_path(values, "bwrap_binary")?;
        let width = integer(values, "width", WIDTH)?;
        let height = integer(values, "height", HEIGHT)?;
        let max_fps = integer(values, "max_fps", MAX_FPS)?;
        // No default: absent means the owner set none.
        let proxy_server = match values.get("proxy_server") {
            None => None,
            Some(Value::String(proxy)) if is_proxy_server(proxy) => Some(proxy.clone()),
            Some(other) => {
                return Err(format!(
                    "config proxy_server must be <scheme>://<host>:<port> with scheme http, \
                     https, socks4 or socks5 and no user name or password, got {other}"
                ));
            }
        };
        Ok(Self {
            chrome_binary,
            bwrap_binary,
            size: (width, height),
            max_fps,
            proxy_server,
        })
    }
}

/// `<scheme>://<host>:<port>`: one proxy for every scheme, as Chrome's
/// `--proxy-server` takes it, with nothing else on the command line.
fn is_proxy_server(proxy: &str) -> bool {
    let Some((scheme, rest)) = proxy.split_once("://") else {
        return false;
    };
    let Some((host, port)) = rest.rsplit_once(':') else {
        return false;
    };
    let host_ok = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(v6) => !v6.is_empty() && v6.bytes().all(|b| b.is_ascii_hexdigit() || b == b':'),
        None => {
            !host.is_empty()
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        }
    };
    ["http", "https", "socks4", "socks5"].contains(&scheme)
        && host_ok
        && port.parse::<u16>().is_ok_and(|p| p > 0)
}

fn absolute_path(values: &Map<String, Value>, key: &str) -> Result<PathBuf, String> {
    match values.get(key) {
        Some(Value::String(path)) if PathBuf::from(path).is_absolute() => Ok(PathBuf::from(path)),
        Some(other) => Err(format!(
            "config {key} must be an absolute path, got {other}"
        )),
        None => Err(format!("config {key} is missing")),
    }
}

fn integer(values: &Map<String, Value>, key: &str, (min, max): (u64, u64)) -> Result<u32, String> {
    match values.get(key) {
        Some(value) => match value.as_u64() {
            Some(n) if (min..=max).contains(&n) => Ok(n as u32),
            _ => Err(format!(
                "config {key} must be an integer from {min} to {max}, got {value}"
            )),
        },
        None => Err(format!("config {key} is missing")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn env(id: &str) -> PluginEnv {
        PluginEnv {
            id: id.into(),
            data_dir: "/data/desktop".into(),
            token: "t".into(),
        }
    }

    #[test]
    fn config_reads_every_key_and_refuses_bad_or_missing_ones() {
        let good = json!({ "chrome_binary": "/opt/chrome/chrome", "bwrap_binary": "/usr/bin/bwrap",
                           "width": 1280, "height": 800, "max_fps": 15 });
        assert_eq!(
            DesktopConfig::from_values(&values(good.clone())).unwrap(),
            DesktopConfig {
                chrome_binary: "/opt/chrome/chrome".into(),
                bwrap_binary: "/usr/bin/bwrap".into(),
                size: (1280, 800),
                max_fps: 15,
                proxy_server: None,
            }
        );
        for (key, bad) in [
            ("chrome_binary", json!("chrome")),
            ("chrome_binary", json!(7)),
            ("bwrap_binary", json!("bwrap")),
            ("bwrap_binary", json!(null)),
            ("width", json!(0)),
            ("width", json!(1280.5)),
            ("height", json!(10_000)),
            ("max_fps", json!(0)),
            ("max_fps", json!("15")),
            ("proxy_server", json!(2080)),
        ] {
            let mut v = good.clone();
            v[key] = bad.clone();
            let err = DesktopConfig::from_values(&values(v)).unwrap_err();
            assert!(err.contains(key), "{key}={bad}: {err}");
        }
        for key in [
            "chrome_binary",
            "bwrap_binary",
            "width",
            "height",
            "max_fps",
        ] {
            let mut v = good.clone();
            v.as_object_mut().unwrap().remove(key);
            let err = DesktopConfig::from_values(&values(v)).unwrap_err();
            assert_eq!(err, format!("config {key} is missing"));
        }
    }

    #[test]
    fn a_proxy_is_one_scheme_host_and_port_and_nothing_else() {
        let with = |proxy: &str| {
            let v = json!({ "chrome_binary": "/c", "bwrap_binary": "/b", "width": 1280,
                            "height": 800, "max_fps": 15, "proxy_server": proxy });
            DesktopConfig::from_values(&values(v)).map(|c| c.proxy_server)
        };
        for good in [
            "http://127.0.0.1:2080",
            "socks5://proxy.lan:1080",
            "https://[::1]:8443",
        ] {
            assert_eq!(with(good).unwrap().as_deref(), Some(good));
        }
        for bad in [
            "127.0.0.1:2080",
            "ftp://h:1",
            "http://user:pw@h:1",
            "http://h",
            "http://h:0",
            "http://h:99999",
            "http://h:1 --no-sandbox",
            "http=h:1;https=h:2",
            "",
        ] {
            assert!(with(bad).unwrap_err().contains("proxy_server"), "{bad:?}");
        }
    }

    /// The `src` must pass the `window` block's allowlist, which the kernel
    /// and the frontend share through `test-data/window-src-v1.json`.
    #[test]
    fn stream_src_matches_the_window_block_pattern() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../test-data/window-src-v1.json")).unwrap();
        let pattern = regex::Regex::new(fixture["pattern"].as_str().unwrap()).unwrap();
        for id in ["desktop", "dev-neige-desktop", "a.b", "d2"] {
            let src = env(id).stream_src(CHROME);
            assert!(pattern.is_match(&src), "{src}");
        }
        assert_eq!(
            env("desktop").stream_src(CHROME),
            "/api/plugins/desktop/ws/apps/chrome/stream"
        );
        // The id is the one the kernel delivered, never a fixed one.
        assert_eq!(
            env("other").stream_src(CHROME),
            "/api/plugins/other/ws/apps/chrome/stream"
        );
    }
}
