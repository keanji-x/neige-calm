//! Deterministic host-side CONNECT gate for the codex-e2e tier (#923 defect 2).
//!
//! This crate replaces the tier's old probabilistic "dead-port fingerprint"
//! fence (which tried to *infer* that prod `127.0.0.1:4040/4041` was
//! unreachable by observing how the operator's shared sing-box proxy answered
//! dead-port probes — an inference we could never make fail-closed because we
//! do not control sing-box's routing/DNS) with a proxy WE author and configure.
//!
//! ## Security model (design §2 INVARIANT — state and preserve)
//!
//! The positive allowlist `host ∈ {dot-anchored chatgpt/openai} ∧ port == 443`
//! is the SOLE gate that admits a target. It is evaluated before and
//! independently of any IP check. Prod is unreachable because it FAILS this
//! positive gate (`:4040/:4041 != 443`, and its host is not chatgpt/openai) —
//! never because an IP classifier happened to catch it. An IP classifier could
//! not catch it anyway: prod binds `0.0.0.0:4040/4041`, so the box's own
//! public IP also serves prod and `is_non_public_ip(<box-public-ip>)` is false.
//!
//! [`is_non_public_ip`] is kept (copied from codex `network-proxy/src/policy.rs`)
//! and unit-tested, but ONLY as a documented log-only tripwire: the datapath
//! sends the allowlisted *hostname* upstream and never resolves, so the
//! classifier is never consulted to admit or deny. It may only ever ADD a log
//! line, never rescue a target.
//!
//! ## Datapath
//!
//! Per client `CONNECT host:port`:
//!   1. `port != 443`            -> `403 Forbidden`, close.
//!   2. host not dot-anchored    -> `403 Forbidden`, close.
//!   3. otherwise forward `CONNECT <hostname>:443 HTTP/1.1` UPSTREAM to
//!      sing-box (`127.0.0.1:2080` by default), read its `200`; relay a `502`
//!      on any non-200. We send the HOSTNAME, not a resolved IP: this box's
//!      resolver returns split-DNS Meta IPs for chatgpt/openai, which would
//!      cause a cert mismatch; sing-box resolves correctly through the tunnel.
//!   4. return `200 Connection established` to the client, then splice.

use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::Ipv6Addr;
use std::time::Duration;

use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::io::copy_bidirectional;
use tokio::net::TcpStream;

/// Default upstream: the operator's shared sing-box HTTP proxy on host
/// loopback. Overridable via argv/env so tests can point it at a stub.
pub const DEFAULT_UPSTREAM: &str = "127.0.0.1:2080";

/// Upper bound on the HTTP head (request/response line + headers up to the
/// terminating CRLFCRLF) we will buffer, so a peer that never sends the
/// terminator cannot make us allocate without limit.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// How long we wait for the client to finish sending its CONNECT head before
/// giving up (408). A silent / slow-loris client cannot pin a task past this.
const CLIENT_HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the whole upstream CONNECT handshake (dial + request + response
/// head) may take before we give up (504). A hung sing-box cannot pin a task.
const UPSTREAM_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

// Fixed responses. `Connection: close` on the error responses so a client that
// pipelines does not wait for a keep-alive that will never come.
const RESP_200: &[u8] = b"HTTP/1.1 200 Connection established\r\n\r\n";
const RESP_400: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_403: &[u8] = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_408: &[u8] =
    b"HTTP/1.1 408 Request Timeout\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_502: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_504: &[u8] =
    b"HTTP/1.1 504 Gateway Timeout\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

// ---------------------------------------------------------------------------
// The load-bearing positive gate.
// ---------------------------------------------------------------------------

/// Dot-anchored host allowlist. Mirrors the *technique* of codex's
/// `codex-client/src/chatgpt_hosts.rs` — an EXACT set plus a leading-dot
/// SUFFIX set — but scoped to the egress domains the ChatGPT-auth tier
/// actually reaches (design §3):
///   - `chatgpt.com`     — `CHATGPT_CODEX_BASE_URL` (SSE + all backend-api paths)
///   - `auth.openai.com` — ChatGPT-auth token refresh/revoke
///   - `api.openai.com`  — API-key path (completeness / future)
///   - `*.chatgpt.com`   — subdomains (e.g. OTEL `ab.chatgpt.com`, off in debug)
///
/// The leading dot in the suffix anchors the match to a real label boundary,
/// so `evilchatgpt.com` and `chatgpt.com.evil.example` are REJECTED.
///
/// `host` MUST already be normalized (lowercased, brackets/port/trailing-dot
/// stripped) — see [`parse_connect_target`].
pub fn is_allowed_host(host: &str) -> bool {
    const EXACT_HOSTS: &[&str] = &["chatgpt.com", "auth.openai.com", "api.openai.com"];
    const SUBDOMAIN_SUFFIXES: &[&str] = &[".chatgpt.com"];

    EXACT_HOSTS.contains(&host)
        || SUBDOMAIN_SUFFIXES
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

/// Split a CONNECT authority (`host:port`) into a normalized host and its port.
///
/// Normalization mirrors codex `policy.rs::normalize_host`: strip one layer of
/// IPv6 brackets (`[::1]` -> `::1`), lowercase, strip trailing dots. The port is
/// the substring after the FINAL `:` (CONNECT always carries an explicit port).
/// Returns `None` for anything that does not parse to `host:u16` — the caller
/// treats that as a hard deny (fail closed).
///
/// Host CHARSET is validated here (design §2 — deny by construction, #923 F5):
/// a bare hostname is `[A-Za-z0-9.-]` only. Without this, [`is_allowed_host`]'s
/// raw `ends_with(".chatgpt.com")` would string-match injection authorities like
/// `127.0.0.1:4040#.chatgpt.com` / `/a/b?x=.chatgpt.com` (and Cyrillic homographs
/// / embedded NUL), ADMIT them, and forward them verbatim upstream — trusting
/// sing-box/Go's parser to reject them. The whole point of this gate is to NOT
/// depend on the upstream parser's leniency, so we reject the charset ourselves.
pub fn parse_connect_target(authority: &str) -> Option<(String, u16)> {
    let authority = authority.trim();
    let (host_raw, port_raw) = authority.rsplit_once(':')?;
    let port: u16 = port_raw.trim().parse().ok()?;
    let host_raw = host_raw.trim();
    let host_raw = host_raw
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host_raw);
    let host = host_raw.trim_end_matches('.').to_ascii_lowercase();
    // Reject empty AND any byte outside the bare-hostname charset: this drops
    // ':' '#' '@' '/' '?' whitespace and all non-ASCII (homograph / NUL) before
    // is_allowed_host ever sees the host. IPv6 literals (which carry ':') are
    // rejected here too — they are never on the allowlist, so denying them is
    // correct, and it keeps "admit" strictly to real dot-anchored hostnames.
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return None;
    }
    Some((host, port))
}

/// The outcome of the gate. `Allow` carries the normalized hostname to send
/// upstream (never a resolved IP — design §4 step 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Allow(String),
    Deny(&'static str),
}

/// THE sole admitting gate (design §2 INVARIANT). Order is load-bearing:
/// unparseable -> deny; `port != 443` -> deny; host not dot-anchored -> deny;
/// otherwise admit the hostname. Both admitting checks are positive; no IP
/// check participates, so nothing an IP classifier could say can rescue a
/// target that fails here.
pub fn gate(target_authority: &str) -> Gate {
    let (host, port) = match parse_connect_target(target_authority) {
        Some(hp) => hp,
        None => return Gate::Deny("unparseable CONNECT authority"),
    };
    if port != 443 {
        return Gate::Deny("port != 443");
    }
    if !is_allowed_host(&host) {
        return Gate::Deny("host not in dot-anchored allowlist");
    }
    Gate::Allow(host)
}

// ---------------------------------------------------------------------------
// Log-only tripwire (NOT the security boundary — see module docs / design §2).
// Copied from codex `network-proxy/src/policy.rs`. The datapath never calls it
// (we send the hostname, never resolve); it is retained + unit-tested to
// document the classification the design cites and to keep a ready tripwire.
// ---------------------------------------------------------------------------

/// True if `ip` is not a globally-routable public address (loopback, private,
/// link-local, CGNAT, TEST-NET, reserved, …).
pub fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_non_public_ipv4(ip),
        IpAddr::V6(ip) => is_non_public_ipv6(ip),
    }
}

fn is_non_public_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ipv4_in_cidr(ip, [0, 0, 0, 0], 8) // "this network" (RFC 1122)
        || ipv4_in_cidr(ip, [100, 64, 0, 0], 10) // CGNAT (RFC 6598)
        || ipv4_in_cidr(ip, [192, 0, 0, 0], 24) // IETF Protocol Assignments (RFC 6890)
        || ipv4_in_cidr(ip, [192, 0, 2, 0], 24) // TEST-NET-1 (RFC 5737)
        || ipv4_in_cidr(ip, [198, 18, 0, 0], 15) // Benchmarking (RFC 2544)
        || ipv4_in_cidr(ip, [198, 51, 100, 0], 24) // TEST-NET-2 (RFC 5737)
        || ipv4_in_cidr(ip, [203, 0, 113, 0], 24) // TEST-NET-3 (RFC 5737)
        || ipv4_in_cidr(ip, [240, 0, 0, 0], 4) // Reserved (RFC 6890)
}

fn ipv4_in_cidr(ip: Ipv4Addr, base: [u8; 4], prefix: u8) -> bool {
    let ip = u32::from(ip);
    let base = u32::from(Ipv4Addr::from(base));
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (ip & mask) == (base & mask)
}

fn is_non_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4() {
        return is_non_public_ipv4(v4) || ip.is_loopback();
    }
    // Explicit range checks (rather than the still-unstable `is_unique_local` /
    // `is_unicast_link_local` helpers) so this compiles on stable rustc.
    let seg0 = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast() // ff00::/8
        || (seg0 & 0xfe00) == 0xfc00 // fc00::/7 unique-local (RFC 4193)
        || (seg0 & 0xffc0) == 0xfe80 // fe80::/10 link-local
}

// ---------------------------------------------------------------------------
// HTTP head parsing (no over-read of tunnel bytes).
// ---------------------------------------------------------------------------

/// Position of the first `needle` in `haystack`, or `None`.
pub fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Read an HTTP head (request/response line + headers) up to AND INCLUDING the
/// terminating CRLFCRLF, without consuming tunnel bytes that follow it.
///
/// Returns `(head, leftover)`: `head` ends with `\r\n\r\n`; `leftover` is any
/// bytes that arrived in the same read past the terminator. For a well-behaved
/// CONNECT peer (which waits for our response before sending the tunnel)
/// `leftover` is empty, but we preserve and forward it for correctness.
pub async fn read_http_head<R>(reader: &mut R) -> std::io::Result<(Vec<u8>, Vec<u8>)>
where
    R: AsyncRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            // The terminator must COMPLETE within the cap. A terminator whose
            // end lands past MAX_HEAD_BYTES means the head is oversize; accepting
            // it (the old bug) let a >16 KiB head through whenever its terminator
            // happened to arrive in the chunk that crossed the cap.
            if pos + 4 > MAX_HEAD_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "HTTP head terminator lands beyond the 16 KiB cap",
                ));
            }
            let leftover = buf.split_off(pos + 4);
            return Ok((buf, leftover));
        }
        // No terminator within the cap window: once we have buffered the cap's
        // worth of bytes without one, any terminator that could still arrive
        // would necessarily end past the cap, so reject now (before reading more
        // — a peer that never terminates cannot make us buffer without limit).
        if buf.len() >= MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "HTTP head reached the 16 KiB cap without a terminator within it",
            ));
        }
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before end of HTTP head",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Extract the CONNECT target authority (`host:port`) from an HTTP head, or
/// `None` if the first line is not a well-formed `CONNECT <authority>
/// <version>` request line.
///
/// The request line MUST be EXACTLY three tokens (#923 F4): method
/// `CONNECT` (case-insensitive, as codex speaks), an authority, and a version in
/// `{HTTP/1.0, HTTP/1.1}`. A missing version, a fourth token, or an unknown
/// version is rejected — we do not silently ignore trailing garbage on the line.
pub fn connect_target_from_head(head: &[u8]) -> Option<String> {
    let line_end = find_subslice(head, b"\r\n")?;
    let line = std::str::from_utf8(&head[..line_end]).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let authority = parts.next()?;
    let version = parts.next()?;
    // Exactly three tokens: a fourth (or more) is malformed -> reject.
    if parts.next().is_some() {
        return None;
    }
    if !method.eq_ignore_ascii_case("CONNECT") {
        return None;
    }
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return None;
    }
    Some(authority.to_string())
}

/// Parse the numeric status code from an HTTP response head (`HTTP/1.1 200 …`).
pub fn status_code_from_head(head: &[u8]) -> Option<u16> {
    let line_end = find_subslice(head, b"\r\n").unwrap_or(head.len());
    let line = std::str::from_utf8(&head[..line_end]).ok()?;
    let mut parts = line.split_whitespace();
    let _version = parts.next()?;
    parts.next()?.parse::<u16>().ok()
}

// ---------------------------------------------------------------------------
// Connection handler.
// ---------------------------------------------------------------------------

fn is_benign_disconnect(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    matches!(
        e.kind(),
        BrokenPipe | ConnectionReset | ConnectionAborted | UnexpectedEof
    )
}

/// Why the upstream CONNECT handshake failed, so the surrounding timeout wrapper
/// stays OUT of the client-write path (a timeout cancellation must never tear a
/// half-written client response).
enum UpstreamError {
    Connect(std::io::Error),
    Io(std::io::Error),
    BadStatus(Option<u16>),
}

/// Serve one client connection end to end: read its CONNECT, apply [`gate`],
/// and on allow chain `CONNECT <hostname>:443` to `upstream_addr` and splice.
///
/// Returns `Ok(())` for every *handled* outcome (400/403/408/502/504 refusal or
/// a cleanly-torn-down tunnel); `Err` only for an unexpected I/O failure worth
/// logging. Generic over the client stream so the integration tests can drive
/// it over a real unix socket.
pub async fn serve_client<C>(client: C, upstream_addr: &str) -> std::io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
{
    serve_client_inner(
        client,
        upstream_addr,
        CLIENT_HEAD_TIMEOUT,
        UPSTREAM_HANDSHAKE_TIMEOUT,
    )
    .await
}

/// [`serve_client`] with the two timeouts injected, so tests can drive the
/// slow-loris and hung-upstream paths in milliseconds instead of the production
/// seconds. The public entry point pins the production values.
async fn serve_client_inner<C>(
    mut client: C,
    upstream_addr: &str,
    client_head_timeout: Duration,
    upstream_handshake_timeout: Duration,
) -> std::io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
{
    // 1. Read the client's CONNECT head without over-reading tunnel bytes,
    //    bounded by a timeout so a silent / slow-loris client cannot pin the
    //    task. Distinguish a timeout (408) from a parse/oversize/EOF error (400).
    let (head, client_leftover) =
        match tokio::time::timeout(client_head_timeout, read_http_head(&mut client)).await {
            Err(_elapsed) => {
                eprintln!("[e2e-egress-proxy] client CONNECT head read timed out -> 408");
                let _ = client.write_all(RESP_408).await;
                let _ = client.flush().await;
                return Ok(());
            }
            Ok(Err(_)) => {
                let _ = client.write_all(RESP_400).await;
                let _ = client.flush().await;
                return Ok(());
            }
            Ok(Ok(v)) => v,
        };

    let authority = match connect_target_from_head(&head) {
        Some(a) => a,
        None => {
            let _ = client.write_all(RESP_400).await;
            let _ = client.flush().await;
            return Ok(());
        }
    };

    // 2. The gate. Deny -> 403, never dialing upstream (so the DENY decision is
    //    deterministic regardless of sing-box).
    let hostname = match gate(&authority) {
        Gate::Allow(host) => host,
        Gate::Deny(reason) => {
            eprintln!("[e2e-egress-proxy] DENY CONNECT {authority}: {reason} -> 403");
            let _ = client.write_all(RESP_403).await;
            let _ = client.flush().await;
            return Ok(());
        }
    };

    // 3. Admitted: chain to the upstream sing-box proxy, bounded by a timeout so
    //    a hung upstream cannot pin the task. The handshake touches ONLY the
    //    upstream socket (never `client`), so its cancellation on timeout can
    //    never leave the client mid-write. Send the HOSTNAME, port fixed to 443.
    let upstream_req = format!("CONNECT {hostname}:443 HTTP/1.1\r\nHost: {hostname}:443\r\n\r\n");
    let handshake = async {
        let mut upstream = TcpStream::connect(upstream_addr)
            .await
            .map_err(UpstreamError::Connect)?;
        upstream
            .write_all(upstream_req.as_bytes())
            .await
            .map_err(UpstreamError::Io)?;
        upstream.flush().await.map_err(UpstreamError::Io)?;
        let (up_head, up_leftover) = read_http_head(&mut upstream)
            .await
            .map_err(UpstreamError::Io)?;
        match status_code_from_head(&up_head) {
            Some(200) => Ok::<_, UpstreamError>((upstream, up_leftover)),
            other => Err(UpstreamError::BadStatus(other)),
        }
    };

    let (mut upstream, up_leftover) = match tokio::time::timeout(
        upstream_handshake_timeout,
        handshake,
    )
    .await
    {
        Err(_elapsed) => {
            eprintln!(
                "[e2e-egress-proxy] upstream {upstream_addr} CONNECT {hostname}:443 handshake timed out -> 504"
            );
            let _ = client.write_all(RESP_504).await;
            let _ = client.flush().await;
            return Ok(());
        }
        Ok(Err(e)) => {
            match e {
                UpstreamError::Connect(err) => eprintln!(
                    "[e2e-egress-proxy] upstream {upstream_addr} connect failed: {err} -> 502"
                ),
                UpstreamError::Io(err) => eprintln!(
                    "[e2e-egress-proxy] upstream {upstream_addr} CONNECT {hostname}:443 head read failed: {err} -> 502"
                ),
                UpstreamError::BadStatus(other) => eprintln!(
                    "[e2e-egress-proxy] upstream refused CONNECT {hostname}:443 (status {other:?}) -> 502"
                ),
            }
            let _ = client.write_all(RESP_502).await;
            let _ = client.flush().await;
            return Ok(());
        }
        Ok(Ok(v)) => v,
    };

    // 4. Tunnel established. Tell the client, flush any pre-read leftovers, splice.
    client.write_all(RESP_200).await?;
    client.flush().await?;
    if !client_leftover.is_empty() {
        upstream.write_all(&client_leftover).await?;
    }
    if !up_leftover.is_empty() {
        client.write_all(&up_leftover).await?;
    }

    match copy_bidirectional(&mut client, &mut upstream).await {
        Ok(_) => Ok(()),
        Err(e) if is_benign_disconnect(&e) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the dot-anchored allowlist (mirrors chatgpt_hosts.rs test) --------

    #[test]
    fn allowlist_admits_codex_egress_hosts() {
        for host in ["chatgpt.com", "auth.openai.com", "api.openai.com"] {
            assert!(is_allowed_host(host), "should admit {host}");
        }
        for host in ["ab.chatgpt.com", "sub.chatgpt.com", "a.b.chatgpt.com"] {
            assert!(is_allowed_host(host), "should admit subdomain {host}");
        }
    }

    #[test]
    fn allowlist_rejects_dot_anchor_tricks_and_others() {
        for host in [
            "evilchatgpt.com",          // no label boundary before chatgpt.com
            "chatgpt.com.evil.example", // suffix trick
            "chat.openai.com",          // not in our scoped set
            "openai.com",
            "10.0.0.1",
            "169.254.169.254",
            "127.0.0.1",
            "example.com",
            "chatgpt.com.", // caller must normalize; raw trailing dot not admitted
        ] {
            assert!(!is_allowed_host(host), "should reject {host}");
        }
    }

    // ---- authority parsing / normalization ---------------------------------

    #[test]
    fn parse_connect_target_normalizes() {
        assert_eq!(
            parse_connect_target("ChatGPT.com:443"),
            Some(("chatgpt.com".to_string(), 443))
        );
        assert_eq!(
            parse_connect_target("chatgpt.com.:443"),
            Some(("chatgpt.com".to_string(), 443))
        );
        // IPv6 literals carry ':', outside the bare-hostname charset -> rejected
        // (they are never on the allowlist, so denying them is correct — F5).
        assert_eq!(parse_connect_target("[::1]:443"), None);
        assert_eq!(
            parse_connect_target("127.0.0.1:4040"),
            Some(("127.0.0.1".to_string(), 4040))
        );
        assert_eq!(parse_connect_target("chatgpt.com"), None); // no port
        assert_eq!(parse_connect_target("chatgpt.com:https"), None); // non-numeric
        assert_eq!(parse_connect_target(":443"), None); // empty host
    }

    // ---- host charset gate (deny by construction, not upstream leniency) ----

    #[test]
    fn parse_connect_target_rejects_non_hostname_charset() {
        // Authorities whose RAW suffix would string-match the allowlist (or a
        // homograph) but whose host carries injection chars / non-ASCII. The
        // charset gate must reject them at parse time (#923 F5) so is_allowed_host
        // never sees them and they are never forwarded upstream verbatim.
        for authority in [
            "127.0.0.1:4040#.chatgpt.com:443",
            "127.0.0.1:4040.chatgpt.com:443",
            "user@127.0.0.1:4040#.chatgpt.com:443",
            "/a/b?x=.chatgpt.com:443",
            "\u{0441}hatgpt.com:443",  // Cyrillic 'с' homograph
            "chatgpt.com\u{0000}:443", // embedded NUL
            "chat gpt.com:443",        // whitespace
            "[::1]:443",               // IPv6 literal (colons)
        ] {
            assert_eq!(
                parse_connect_target(authority),
                None,
                "charset gate must reject {authority:?}"
            );
        }
        // And the gate turns such an authority into an explicit Deny, not Allow.
        assert_eq!(
            gate("127.0.0.1:4040#.chatgpt.com:443"),
            Gate::Deny("unparseable CONNECT authority")
        );
    }

    // ---- the gate (order + INVARIANT) --------------------------------------

    #[test]
    fn gate_admits_only_allowlisted_host_on_443() {
        assert_eq!(
            gate("chatgpt.com:443"),
            Gate::Allow("chatgpt.com".to_string())
        );
        assert_eq!(
            gate("sub.chatgpt.com:443"),
            Gate::Allow("sub.chatgpt.com".to_string())
        );
        assert_eq!(
            gate("auth.openai.com:443"),
            Gate::Allow("auth.openai.com".to_string())
        );
    }

    #[test]
    fn gate_denies_prod_ports_on_port_check() {
        // Prod fails the POSITIVE gate on PORT, before any host/IP consideration.
        assert_eq!(gate("127.0.0.1:4040"), Gate::Deny("port != 443"));
        assert_eq!(gate("127.0.0.1:4041"), Gate::Deny("port != 443"));
        // Even an allowlisted host on a non-443 port is denied.
        assert_eq!(gate("chatgpt.com:80"), Gate::Deny("port != 443"));
    }

    #[test]
    fn gate_denies_non_allowlisted_host_on_443() {
        for t in [
            "10.0.0.1:443",
            "169.254.169.254:443",
            "192.168.1.1:443",
            "evilchatgpt.com:443",
            "chatgpt.com.evil.example:443",
            "chat.openai.com:443",
        ] {
            assert_eq!(
                gate(t),
                Gate::Deny("host not in dot-anchored allowlist"),
                "expected host-deny for {t}"
            );
        }
    }

    #[test]
    fn gate_denies_unparseable() {
        assert_eq!(gate("garbage"), Gate::Deny("unparseable CONNECT authority"));
    }

    #[test]
    fn gate_allows_trailing_dot_exact_host_as_intentional() {
        // A trailing-dot FQDN normalizes to the same host (matches codex
        // chatgpt_hosts behavior); this locks it as INTENTIONALLY allowed (F4).
        assert_eq!(
            gate("auth.openai.com.:443"),
            Gate::Allow("auth.openai.com".to_string())
        );
        assert_eq!(
            gate("chatgpt.com.:443"),
            Gate::Allow("chatgpt.com".to_string())
        );
        // A trailing-dot subdomain also normalizes and is admitted by the suffix.
        assert_eq!(
            gate("ab.chatgpt.com.:443"),
            Gate::Allow("ab.chatgpt.com".to_string())
        );
    }

    // ---- is_non_public_ip (log-only tripwire; parity with codex policy.rs) --

    #[test]
    fn is_non_public_ip_rejects_private_loopback_linklocal() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "192.168.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.1.2.3",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
        ] {
            assert!(
                is_non_public_ip(ip.parse().unwrap()),
                "{ip} should be non-public"
            );
        }
        for ip in ["8.8.8.8", "1.1.1.1", "::ffff:8.8.8.8"] {
            assert!(
                !is_non_public_ip(ip.parse().unwrap()),
                "{ip} should be public"
            );
        }
    }

    // ---- HTTP head parsing --------------------------------------------------

    #[test]
    fn find_subslice_locates_terminator() {
        assert_eq!(find_subslice(b"ab\r\n\r\ncd", b"\r\n\r\n"), Some(2));
        assert_eq!(find_subslice(b"abcd", b"\r\n\r\n"), None);
        assert_eq!(find_subslice(b"", b"x"), None);
    }

    #[test]
    fn connect_target_parses_request_line() {
        let head = b"CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com:443\r\n\r\n";
        assert_eq!(
            connect_target_from_head(head).as_deref(),
            Some("chatgpt.com:443")
        );
        // Case-insensitive method.
        let head2 = b"connect a.b:443 HTTP/1.1\r\n\r\n";
        assert_eq!(connect_target_from_head(head2).as_deref(), Some("a.b:443"));
        // Non-CONNECT method -> None.
        let get = b"GET http://x/ HTTP/1.1\r\n\r\n";
        assert_eq!(connect_target_from_head(get), None);
    }

    #[test]
    fn connect_target_requires_exactly_three_tokens_and_known_version() {
        // Exactly three tokens, `CONNECT <authority> HTTP/1.{0,1}` (F4).
        assert_eq!(
            connect_target_from_head(b"CONNECT a:443 HTTP/1.1\r\n\r\n").as_deref(),
            Some("a:443")
        );
        assert_eq!(
            connect_target_from_head(b"CONNECT a:443 HTTP/1.0\r\n\r\n").as_deref(),
            Some("a:443")
        );
        // Missing the version token (two tokens) -> reject.
        assert_eq!(connect_target_from_head(b"CONNECT a:443\r\n\r\n"), None);
        // A fourth token -> reject (no silently-ignored trailing garbage).
        assert_eq!(
            connect_target_from_head(b"CONNECT a:443 HTTP/1.1 extra\r\n\r\n"),
            None
        );
        // Unknown / unsupported versions -> reject.
        assert_eq!(
            connect_target_from_head(b"CONNECT a:443 HTTP/2.0\r\n\r\n"),
            None
        );
        assert_eq!(connect_target_from_head(b"CONNECT a:443 xyz\r\n\r\n"), None);
    }

    #[test]
    fn status_code_parses_response_line() {
        assert_eq!(
            status_code_from_head(b"HTTP/1.1 200 Connection established\r\n\r\n"),
            Some(200)
        );
        assert_eq!(
            status_code_from_head(b"HTTP/1.1 502 Bad Gateway\r\n\r\n"),
            Some(502)
        );
        assert_eq!(status_code_from_head(b"garbage\r\n\r\n"), None);
    }

    #[tokio::test]
    async fn read_http_head_splits_head_from_tunnel_leftover() {
        use std::io::Cursor;
        // Bytes past the CRLFCRLF must be returned as leftover, not swallowed.
        let mut cur = Cursor::new(b"CONNECT a:443 HTTP/1.1\r\n\r\nTUNNEL".to_vec());
        let (head, leftover) = read_http_head(&mut cur).await.unwrap();
        assert!(head.ends_with(b"\r\n\r\n"));
        assert_eq!(&leftover, b"TUNNEL");
    }

    // ---- HTTP head cap enforcement (#923 F2) --------------------------------

    #[tokio::test]
    async fn read_http_head_accepts_head_exactly_at_cap() {
        use std::io::Cursor;
        // A head whose total size INCLUDING the CRLFCRLF is exactly the cap is
        // accepted — the defined boundary.
        let mut data = vec![b'a'; MAX_HEAD_BYTES];
        let n = data.len();
        data[n - 4..].copy_from_slice(b"\r\n\r\n");
        let mut cur = Cursor::new(data);
        let (head, leftover) = read_http_head(&mut cur).await.unwrap();
        assert_eq!(head.len(), MAX_HEAD_BYTES);
        assert!(head.ends_with(b"\r\n\r\n"));
        assert!(leftover.is_empty());
    }

    #[tokio::test]
    async fn read_http_head_rejects_oversize_head_with_late_terminator() {
        use std::io::Cursor;
        // 17 KiB of header bytes THEN the terminator: the buffer reaches the cap
        // with no terminator within it, so it is rejected — the late terminator
        // (well past the cap) is never accepted.
        let mut data = vec![b'a'; 17 * 1024];
        data.extend_from_slice(b"\r\n\r\n");
        let mut cur = Cursor::new(data);
        let err = read_http_head(&mut cur).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn read_http_head_rejects_terminator_straddling_cap() {
        // The exact bug: a terminator that lands in the chunk crossing the cap
        // must be rejected by the `pos + 4 > cap` check, not accepted. Drive it
        // with a reader that lands the buffer one byte under the cap and then
        // delivers a chunk whose terminator ends past the cap.
        use std::collections::VecDeque;
        use std::pin::Pin;
        use std::task::{Context, Poll};
        use tokio::io::ReadBuf;

        struct Scripted(VecDeque<Vec<u8>>);
        impl AsyncRead for Scripted {
            fn poll_read(
                mut self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<std::io::Result<()>> {
                if let Some(front) = self.0.front_mut() {
                    let n = front.len().min(buf.remaining());
                    buf.put_slice(&front[..n]);
                    front.drain(..n);
                    if front.is_empty() {
                        self.0.pop_front();
                    }
                }
                Poll::Ready(Ok(()))
            }
        }

        // MAX_HEAD_BYTES-1 filler (buffer lands 1 under the cap), then "aa\r\n\r\n":
        // the terminator starts at index cap+1, so pos+4 = cap+5 > cap -> reject.
        let mut chunks = VecDeque::new();
        chunks.push_back(vec![b'a'; MAX_HEAD_BYTES - 1]);
        chunks.push_back(b"aa\r\n\r\n".to_vec());
        let mut reader = Scripted(chunks);
        let err = read_http_head(&mut reader).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    // ---- per-connection timeouts (#923 F3) ----------------------------------

    #[tokio::test]
    async fn serve_client_times_out_silent_client_with_408() {
        // A client that connects but never sends its CONNECT head must not pin
        // the task: the head-read timeout fires, we answer 408, and return Ok.
        let (client, mut peer) = tokio::io::duplex(1024);
        // Keep `peer` alive (never write) so the read pends rather than EOFs.
        let res = serve_client_inner(
            client,
            "127.0.0.1:9",
            Duration::from_millis(50),
            Duration::from_secs(30),
        )
        .await;
        assert!(res.is_ok(), "handler must return Ok on client timeout");
        let mut got = Vec::new();
        peer.read_to_end(&mut got).await.unwrap();
        assert!(
            got.starts_with(b"HTTP/1.1 408"),
            "expected 408, got {:?}",
            String::from_utf8_lossy(&got)
        );
    }

    #[tokio::test]
    async fn serve_client_times_out_hung_upstream_with_504() {
        // Upstream that accepts but never answers the CONNECT -> the handshake
        // timeout fires and we answer 504.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let _accepted = listener.accept().await; // hold it, never respond
            std::future::pending::<()>().await;
        });
        let (client, mut peer) = tokio::io::duplex(4096);
        // Client sends a VALID, allowlisted CONNECT so the gate admits and we
        // dial the (hung) upstream.
        peer.write_all(b"CONNECT chatgpt.com:443 HTTP/1.1\r\nHost: chatgpt.com:443\r\n\r\n")
            .await
            .unwrap();
        let res = serve_client_inner(
            client,
            &addr,
            Duration::from_secs(30),
            Duration::from_millis(50),
        )
        .await;
        assert!(res.is_ok(), "handler must return Ok on upstream timeout");
        let mut got = Vec::new();
        peer.read_to_end(&mut got).await.unwrap();
        assert!(
            got.starts_with(b"HTTP/1.1 504"),
            "expected 504, got {:?}",
            String::from_utf8_lossy(&got)
        );
    }
}
