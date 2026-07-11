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

// Fixed responses. `Connection: close` on the error responses so a client that
// pipelines does not wait for a keep-alive that will never come.
const RESP_200: &[u8] = b"HTTP/1.1 200 Connection established\r\n\r\n";
const RESP_400: &[u8] = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_403: &[u8] = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESP_502: &[u8] = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

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
/// IPv6 brackets (`[::1]` -> `::1`), lowercase, strip a single trailing dot.
/// The port is the substring after the FINAL `:` (CONNECT always carries an
/// explicit port). Returns `None` for anything that does not parse to
/// `host:u16` — the caller treats that as a hard deny (fail closed).
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
    if host.is_empty() {
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
            let leftover = buf.split_off(pos + 4);
            return Ok((buf, leftover));
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "HTTP head exceeded 16 KiB without a CRLFCRLF terminator",
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
/// `None` if the first line is not a `CONNECT <authority> ...` request line.
pub fn connect_target_from_head(head: &[u8]) -> Option<String> {
    let line_end = find_subslice(head, b"\r\n")?;
    let line = std::str::from_utf8(&head[..line_end]).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    if !method.eq_ignore_ascii_case("CONNECT") {
        return None;
    }
    let authority = parts.next()?;
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

/// Serve one client connection end to end: read its CONNECT, apply [`gate`],
/// and on allow chain `CONNECT <hostname>:443` to `upstream_addr` and splice.
///
/// Returns `Ok(())` for every *handled* outcome (400/403/502 refusal or a
/// cleanly-torn-down tunnel); `Err` only for an unexpected I/O failure worth
/// logging. Generic over the client stream so the integration tests can drive
/// it over a real unix socket.
pub async fn serve_client<C>(mut client: C, upstream_addr: &str) -> std::io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
{
    // 1. Read the client's CONNECT head without over-reading tunnel bytes.
    let (head, client_leftover) = match read_http_head(&mut client).await {
        Ok(v) => v,
        Err(_) => {
            let _ = client.write_all(RESP_400).await;
            let _ = client.flush().await;
            return Ok(());
        }
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

    // 3. Admitted: chain to the upstream sing-box proxy. Send the HOSTNAME,
    //    port fixed to 443 (the gate rejected any other port).
    let mut upstream = match TcpStream::connect(upstream_addr).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[e2e-egress-proxy] upstream {upstream_addr} connect failed: {e} -> 502");
            let _ = client.write_all(RESP_502).await;
            let _ = client.flush().await;
            return Ok(());
        }
    };

    let upstream_req = format!("CONNECT {hostname}:443 HTTP/1.1\r\nHost: {hostname}:443\r\n\r\n");
    upstream.write_all(upstream_req.as_bytes()).await?;
    upstream.flush().await?;

    let (up_head, up_leftover) = match read_http_head(&mut upstream).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "[e2e-egress-proxy] upstream {upstream_addr} CONNECT {hostname}:443 head read failed: {e} -> 502"
            );
            let _ = client.write_all(RESP_502).await;
            let _ = client.flush().await;
            return Ok(());
        }
    };
    match status_code_from_head(&up_head) {
        Some(200) => {}
        other => {
            eprintln!(
                "[e2e-egress-proxy] upstream refused CONNECT {hostname}:443 (status {other:?}) -> 502"
            );
            let _ = client.write_all(RESP_502).await;
            let _ = client.flush().await;
            return Ok(());
        }
    }

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
            "evilchatgpt.com",       // no label boundary before chatgpt.com
            "chatgpt.com.evil.example", // suffix trick
            "chat.openai.com",       // not in our scoped set
            "openai.com",
            "10.0.0.1",
            "169.254.169.254",
            "127.0.0.1",
            "example.com",
            "chatgpt.com.",          // caller must normalize; raw trailing dot not admitted
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
        assert_eq!(
            parse_connect_target("[::1]:443"),
            Some(("::1".to_string(), 443))
        );
        assert_eq!(
            parse_connect_target("127.0.0.1:4040"),
            Some(("127.0.0.1".to_string(), 4040))
        );
        assert_eq!(parse_connect_target("chatgpt.com"), None); // no port
        assert_eq!(parse_connect_target("chatgpt.com:https"), None); // non-numeric
        assert_eq!(parse_connect_target(":443"), None); // empty host
    }

    // ---- the gate (order + INVARIANT) --------------------------------------

    #[test]
    fn gate_admits_only_allowlisted_host_on_443() {
        assert_eq!(gate("chatgpt.com:443"), Gate::Allow("chatgpt.com".to_string()));
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
}
