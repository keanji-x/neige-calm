//! The five tools. The kernel mints them `plugin_<id>_<tool>` and sends the
//! plugin the names below.

use std::sync::Arc;
use std::time::Duration;

use compositor::{InputEvent, WindowId, WindowInfo};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Desktop;
use crate::app::AppError;
use crate::capture;
use crate::config::CHROME;
use crate::keys;

/// The tools this binary serves; the manifest's `exposes_tools` must match.
pub const TOOLS: [&str; 5] = [
    "window_ls",
    "window_cat",
    "window_input",
    "page_open",
    "page_cat",
];

/// `page_open` waits this long for the load event.
pub const PAGE_OPEN_TIMEOUT: Duration = Duration::from_secs(20);
/// `page_cat` text cap, in UTF-16 code units, applied inside the page.
pub const PAGE_TEXT_LIMIT: u32 = 100_000;
/// After `window_input`, the window keeps the watched frame rate this long so
/// it draws the result promptly.
pub const INPUT_PACE_HOLD: Duration = Duration::from_secs(1);
/// The pause between two of a call's events, as a person leaves: an app may
/// act on one before the next (`ctrl+l` moves Chrome's focus to the address
/// bar only after a moment, and text sent at once would miss it).
pub const INPUT_GAP: Duration = Duration::from_millis(50);
/// Most input events one `window_input` call may expand to.
pub const MAX_INPUT_EVENTS: usize = 8_000;

/// A JSON-RPC error for one tool call (`docs/conventions/agent-commands.md` §5).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

pub const INVALID_PARAMS: i64 = -32602;
pub const NOT_FOUND: i64 = -32404;
pub const CONFLICT: i64 = -32409;
pub const UNAVAILABLE: i64 = -32503;
pub const INTERNAL: i64 = -32603;
pub const UNKNOWN_TOOL: i64 = -32601;

impl ToolError {
    pub fn new(code: i64, tool: &str, message: impl std::fmt::Display) -> Self {
        Self {
            code,
            message: format!("{tool}: {message}"),
            data: None,
        }
    }

    fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// Runs one `tools/call`; the result is an MCP `CallToolResult`.
pub async fn call(desktop: &Arc<Desktop>, tool: &str, args: Value) -> Result<Value, ToolError> {
    match tool {
        "window_ls" => {
            parse::<NoArgs>(tool, args)?;
            Ok(result(window_ls(desktop)))
        }
        "window_cat" => window_cat(desktop, parse(tool, args)?).await,
        "window_input" => window_input(desktop, parse(tool, args)?).await,
        "page_open" => page_open(desktop, parse(tool, args)?).await,
        "page_cat" => {
            parse::<NoArgs>(tool, args)?;
            page_cat(desktop).await
        }
        other => Err(ToolError::new(
            UNKNOWN_TOOL,
            other,
            format!("unknown tool; this plugin serves {}", TOOLS.join(", ")),
        )),
    }
}

fn parse<T: serde::de::DeserializeOwned>(tool: &str, args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::new(INVALID_PARAMS, tool, e))
}

/// A successful result: the JSON as text, and as structured content.
fn result(structured: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": structured.to_string() }],
        "structuredContent": structured,
        "isError": false,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

fn window_ls(desktop: &Desktop) -> Value {
    let pid = desktop.app.state().pid;
    let windows: Vec<Value> = desktop
        .windows
        .list()
        .into_iter()
        .map(|w| {
            let app = (u32::try_from(w.pid).ok() == pid && pid.is_some()).then_some(CHROME);
            json!({
                "window_id": w.id.0,
                "app": app,
                "title": w.title,
                "width": w.size.0,
                "height": w.size.1,
            })
        })
        .collect();
    json!({
        "windows": windows,
        "apps": [{
            "app": CHROME,
            "running": pid.is_some(),
            "src": desktop.env.stream_src(CHROME),
        }],
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatArgs {
    window_id: u64,
}

fn open_window(desktop: &Desktop, tool: &str, id: u64) -> Result<WindowInfo, ToolError> {
    desktop.windows.get(WindowId(id)).ok_or_else(|| {
        ToolError::new(
            NOT_FOUND,
            tool,
            format!("window {id} is not open; window_ls lists the open windows"),
        )
    })
}

async fn window_cat(desktop: &Arc<Desktop>, args: CatArgs) -> Result<Value, ToolError> {
    let tool = "window_cat";
    let window = open_window(desktop, tool, args.window_id)?;
    let compositor = desktop.compositor.clone();
    let shot = blocking(tool, move || capture::fresh_png(&compositor, window.id)).await?;
    let shot = shot.map_err(|e| compositor_error(tool, e))?;
    let title = desktop
        .windows
        .get(window.id)
        .map_or(window.title, |w| w.title);
    let meta = json!({
        "window_id": window.id.0,
        "title": title,
        "width": shot.size.0,
        "height": shot.size.1,
    });
    Ok(json!({
        "content": [
            { "type": "image", "data": shot.base64_png, "mimeType": "image/png" },
            { "type": "text", "text": meta.to_string() },
        ],
        "structuredContent": meta,
        "isError": false,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputArgs {
    window_id: u64,
    events: Vec<EventArg>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum EventArg {
    Click {
        x: f64,
        y: f64,
        #[serde(default)]
        button: Button,
    },
    Move {
        x: f64,
        y: f64,
    },
    Scroll {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    Key {
        keys: String,
    },
    Text {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Button {
    #[default]
    Left,
    Middle,
    Right,
}

impl Button {
    /// The evdev code, through `window-stream`'s browser button table.
    fn evdev(self) -> u32 {
        let index = match self {
            Button::Left => 0,
            Button::Middle => 1,
            Button::Right => 2,
        };
        window_stream::evdev_button(index).expect("browser buttons 0-2 map to evdev")
    }
}

/// Expands the events into compositor input for a window of `size`: one
/// batch per event, in order.
pub(crate) fn input_events(
    events: &[EventArg],
    size: (u32, u32),
) -> Result<Vec<Vec<InputEvent>>, String> {
    if events.is_empty() {
        return Err("events is empty".into());
    }
    let point = |x: f64, y: f64| -> Result<InputEvent, String> {
        let inside = |v: f64, side: u32| v.is_finite() && v >= 0.0 && v < f64::from(side);
        if inside(x, size.0) && inside(y, size.1) {
            Ok(InputEvent::Motion { x, y })
        } else {
            Err(format!(
                "({x}, {y}) is outside the {}x{} window",
                size.0, size.1
            ))
        }
    };
    let mut batches = Vec::new();
    let mut total = 0;
    for (i, event) in events.iter().enumerate() {
        let at = |e: String| format!("events[{i}]: {e}");
        let mut out = Vec::new();
        match event {
            EventArg::Click { x, y, button } => {
                out.push(point(*x, *y).map_err(at)?);
                let code = button.evdev();
                out.push(InputEvent::Button {
                    code,
                    pressed: true,
                });
                out.push(InputEvent::Button {
                    code,
                    pressed: false,
                });
            }
            EventArg::Move { x, y } => out.push(point(*x, *y).map_err(at)?),
            EventArg::Scroll { x, y, dx, dy } => {
                out.push(point(*x, *y).map_err(at)?);
                let distance = |v: f64| {
                    if v.is_finite() && v.abs() <= window_stream::MAX_WHEEL {
                        Ok(v)
                    } else {
                        Err(at(format!(
                            "scroll distances are finite pixels up to {}",
                            window_stream::MAX_WHEEL
                        )))
                    }
                };
                out.push(InputEvent::Axis {
                    dx: distance(*dx)?,
                    dy: distance(*dy)?,
                });
            }
            EventArg::Key { keys } => out.extend(keys::chord(keys).map_err(|e| at(e.0))?),
            EventArg::Text { text } => out.extend(keys::text(text).map_err(|e| at(e.0))?),
        }
        total += out.len();
        if total > MAX_INPUT_EVENTS {
            return Err(format!(
                "more than {MAX_INPUT_EVENTS} key and pointer events; split the input over several calls"
            ));
        }
        batches.push(out);
    }
    Ok(batches)
}

async fn window_input(desktop: &Arc<Desktop>, args: InputArgs) -> Result<Value, ToolError> {
    let tool = "window_input";
    let window = open_window(desktop, tool, args.window_id)?;
    let batches = input_events(&args.events, window.size)
        .map_err(|e| ToolError::new(INVALID_PARAMS, tool, e))?;
    let compositor = desktop.compositor.clone();
    let id = window.id;
    // Watch first, so the window draws the input's effect at the watched rate.
    let watch = blocking(tool, move || {
        let watch = compositor.watch(id)?;
        for (i, batch) in batches.into_iter().enumerate() {
            if i > 0 {
                std::thread::sleep(INPUT_GAP);
            }
            compositor.input(id, batch)?;
        }
        Ok(watch)
    })
    .await?
    .map_err(|e| compositor_error(tool, e))?;
    tokio::spawn(async move {
        tokio::time::sleep(INPUT_PACE_HOLD).await;
        drop(watch);
    });
    Ok(result(json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenArgs {
    url: String,
}

/// Only web pages: `file:`, `chrome:` and the like would read the host.
pub(crate) fn check_url(url: &str) -> Result<(), String> {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let web = lower.starts_with("http://") || lower.starts_with("https://");
    let host_part = url.split_once("://").map_or("", |(_, rest)| rest);
    if !web || host_part.is_empty() || host_part.starts_with('/') {
        return Err(format!(
            "`{url}` is not an absolute http:// or https:// URL"
        ));
    }
    if url.len() > 8192 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the URL holds whitespace or control characters, or is over 8192 bytes".into());
    }
    Ok(())
}

async fn page_open(desktop: &Arc<Desktop>, args: OpenArgs) -> Result<Value, ToolError> {
    let tool = "page_open";
    check_url(&args.url).map_err(|e| ToolError::new(INVALID_PARAMS, tool, e))?;
    let navigated = desktop
        .app
        .navigate(args.url, PAGE_OPEN_TIMEOUT)
        .await
        .map_err(|e| app_error(tool, e))?;
    Ok(result(json!({
        "url": navigated.url,
        "title": navigated.title,
        "loaded": navigated.loaded,
    })))
}

async fn page_cat(desktop: &Arc<Desktop>) -> Result<Value, ToolError> {
    let tool = "page_cat";
    let page = desktop
        .app
        .read(PAGE_TEXT_LIMIT)
        .await
        .map_err(|e| app_error(tool, e))?;
    Ok(result(json!({
        "url": page.url,
        "title": page.title,
        "text": page.text,
        "text_truncated": page.text_truncated,
    })))
}

/// Runs a blocking compositor call off the async workers.
async fn blocking<T: Send + 'static>(
    tool: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| ToolError::new(INTERNAL, tool, e))
}

pub(crate) fn compositor_error(tool: &str, error: compositor::Error) -> ToolError {
    let code = match error {
        compositor::Error::WindowGone(_) => NOT_FOUND,
        compositor::Error::NoBuffer(_) | compositor::Error::Stopped => UNAVAILABLE,
        compositor::Error::Setup(_) | compositor::Error::Render(..) => INTERNAL,
    };
    ToolError::new(code, tool, error)
}

pub(crate) fn app_error(tool: &str, error: AppError) -> ToolError {
    use chrome_control::Error as E;
    match error {
        AppError::NotRunning | AppError::ShuttingDown => {
            ToolError::new(UNAVAILABLE, tool, &error)
        }
        AppError::Chrome(E::NoVisiblePage { pages }) => ToolError::new(
            CONFLICT,
            tool,
            format!(
                "no tab is visible; pages: {}. Show one tab (close dialogs and extra windows) and retry",
                describe(&pages)
            ),
        )
        .with_data(json!({ "refusal": "no_visible_page", "pages": pages })),
        AppError::Chrome(E::AmbiguousPage { pages }) => ToolError::new(
            CONFLICT,
            tool,
            format!(
                "more than one tab may be visible (a second window, a popup, or a page that did not \
                 answer, such as one showing a dialog): {}. Close the extra ones and retry",
                describe(&pages)
            ),
        )
        .with_data(json!({ "refusal": "ambiguous_page", "pages": pages })),
        AppError::Chrome(e @ E::Cdp { .. }) => ToolError::new(INTERNAL, tool, e),
        AppError::Chrome(e) => ToolError::new(UNAVAILABLE, tool, e),
    }
}

fn describe(pages: &[chrome_control::PageInfo]) -> String {
    if pages.is_empty() {
        return "none".into();
    }
    pages
        .iter()
        .map(|p| {
            let visibility = match p.visibility {
                chrome_control::Visibility::Visible => "visible",
                chrome_control::Visibility::Hidden => "hidden",
                chrome_control::Visibility::Unknown => "unknown",
            };
            format!("{} ({visibility}) {:?} {}", p.target_id, p.title, p.url)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
