//! The Track raw route's HTML page response (#2585), for the report `html` block and the file
//! viewer. The page runs in a header sandbox with scripts and nothing else, so even opened
//! top-level in its own tab it is an opaque origin: it cannot read the session cookie or call the
//! API, and `default-src 'none'` refuses every fetch, socket and external resource. Images keep
//! their own response (`read_file_raw_response_from_handle`), which Planner attachments share.

use super::{CalmError, MAX_READFILE_BYTES, OpenWorkspaceFile, Result, map_io_err};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use std::path::Path;

/// Inline scripts, styles and `data:`/`blob:` media only; no network, forms or `<base>`.
pub(super) const HTML_PAGE_CSP: &str = "sandbox allow-scripts; default-src 'none'; \
     script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; \
     media-src data: blob:; form-action 'none'; base-uri 'none'";

/// `.html` / `.htm`, case-insensitive.
pub(super) fn is_html_page(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
}

/// The cap is the text read cap: the frontend reads the page as text first, and shows the frame
/// only after that read succeeds.
pub(super) async fn html_page_response(opened: OpenWorkspaceFile) -> Result<Response> {
    let too_large = || CalmError::BadRequest("HTML page exceeds 2 MiB cap".into());
    if opened.size > MAX_READFILE_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(opened.size as usize);
    use tokio::io::AsyncReadExt;
    opened
        .file
        .take(MAX_READFILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| map_io_err(&opened.display_path, error))?;
    if bytes.len() as u64 > MAX_READFILE_BYTES {
        return Err(too_large());
    }
    Ok((
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_SECURITY_POLICY, HTML_PAGE_CSP),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        bytes,
    )
        .into_response())
}
