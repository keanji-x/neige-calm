//! Deterministic host-side CONNECT gate for the codex-e2e tier.
//! INVARIANT: the positive allowlist `host ∈ {dot-anchored chatgpt/openai} ∧ port == 443` is the SOLE
//! gate; no IP check participates. The allowlisted HOSTNAME is sent upstream, never a resolved IP.

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

/// Default upstream: the operator's shared sing-box HTTP proxy on host loopback.
pub const DEFAULT_UPSTREAM: &str = "127.0.0.1:2080";

/// Upper bound on the HTTP head we will buffer, so a peer that never sends the terminator cannot
/// make us allocate without limit.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// How long we wait for the client's CONNECT head before giving up (408).
const CLIENT_HEAD_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the whole upstream CONNECT handshake may take before we give up (504).
const UPSTREAM_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

// Fixed responses. `Connection: close` on the error responses so a pipelining client does not wait for a keep-alive.
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

/// Dot-anchored host allowlist: an EXACT set plus a leading-dot SUFFIX set, so `evilchatgpt.com`
/// and `chatgpt.com.evil.example` are REJECTED. `host` MUST already be normalized ([`parse_connect_target`]).
pub fn is_allowed_host(host: &str) -> bool {
    const EXACT_HOSTS: &[&str] = &["chatgpt.com", "auth.openai.com", "api.openai.com"];
    const SUBDOMAIN_SUFFIXES: &[&str] = &[".chatgpt.com"];

    EXACT_HOSTS.contains(&host)
        || SUBDOMAIN_SUFFIXES
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

/// Split a CONNECT authority (`host:port`) into a normalized host and its port; `None` is a hard
/// deny. Host charset is validated here so the suffix match can never admit an injection authority
/// like `127.0.0.1:4040#.chatgpt.com` or a homograph.
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
    // Reject any byte outside the bare-hostname charset before is_allowed_host sees the host; IPv6
    // literals carry ':' and are never on the allowlist.
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return None;
    }
    Some((host, port))
}

/// The outcome of the gate. `Allow` carries the normalized hostname to send upstream (never a resolved IP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Allow(String),
    Deny(&'static str),
}

/// THE sole admitting gate. Order is load-bearing: unparseable -> deny; `port != 443` -> deny; host
/// not dot-anchored -> deny; otherwise admit the hostname. No IP check participates.
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

// Log-only tripwire, NOT the security boundary: the datapath never calls it.

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
    // Explicit range checks so this compiles on stable rustc.
    let seg0 = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast() // ff00::/8
        || (seg0 & 0xfe00) == 0xfc00 // fc00::/7 unique-local (RFC 4193)
        || (seg0 & 0xffc0) == 0xfe80 // fe80::/10 link-local
}

/// Position of the first `needle` in `haystack`, or `None`.
pub fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Read an HTTP head up to AND INCLUDING the terminating CRLFCRLF, without consuming tunnel bytes
/// that follow it; returns `(head, leftover)`.
pub async fn read_http_head<R>(reader: &mut R) -> std::io::Result<(Vec<u8>, Vec<u8>)>
where
    R: AsyncRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            // The terminator must COMPLETE within the cap; one ending past MAX_HEAD_BYTES means the head is oversize.
            if pos + 4 > MAX_HEAD_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "HTTP head terminator lands beyond the 16 KiB cap",
                ));
            }
            let leftover = buf.split_off(pos + 4);
            return Ok((buf, leftover));
        }
        // No terminator within the cap window: any terminator still to arrive would end past the cap, so
        // reject before reading more.
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

/// Extract the CONNECT target authority from an HTTP head, or `None`. The request line MUST be
/// EXACTLY three tokens: `CONNECT` (case-insensitive), an authority, and `HTTP/1.0|1.1`.
pub fn connect_target_from_head(head: &[u8]) -> Option<String> {
    let line_end = find_subslice(head, b"\r\n")?;
    let line = std::str::from_utf8(&head[..line_end]).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let authority = parts.next()?;
    let version = parts.next()?;
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

fn is_benign_disconnect(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    matches!(
        e.kind(),
        BrokenPipe | ConnectionReset | ConnectionAborted | UnexpectedEof
    )
}

/// Why the upstream CONNECT handshake failed, so the timeout wrapper stays OUT of the client-write
/// path (a timeout cancellation must never tear a half-written client response).
enum UpstreamError {
    Connect(std::io::Error),
    Io(std::io::Error),
    BadStatus(Option<u16>),
}

/// Serve one client connection end to end: read its CONNECT, apply [`gate`], and on allow chain
/// `CONNECT <hostname>:443` to `upstream_addr` and splice. `Err` only for an unexpected I/O failure.
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

/// [`serve_client`] with the two timeouts injected so tests can drive the slow-loris and hung-upstream paths.
async fn serve_client_inner<C>(
    mut client: C,
    upstream_addr: &str,
    client_head_timeout: Duration,
    upstream_handshake_timeout: Duration,
) -> std::io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
{
    // Read the CONNECT head bounded by a timeout; a timeout is 408, a parse/oversize/EOF error 400.
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

    // Deny -> 403 without dialing upstream, so the DENY decision is deterministic regardless of sing-box.
    let hostname = match gate(&authority) {
        Gate::Allow(host) => host,
        Gate::Deny(reason) => {
            eprintln!("[e2e-egress-proxy] DENY CONNECT {authority}: {reason} -> 403");
            let _ = client.write_all(RESP_403).await;
            let _ = client.flush().await;
            return Ok(());
        }
    };

    // The handshake touches ONLY the upstream socket, so its cancellation on timeout can never leave
    // the client mid-write. Send the HOSTNAME, port fixed to 443.
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
        assert_eq!(parse_connect_target("[::1]:443"), None);
        assert_eq!(
            parse_connect_target("127.0.0.1:4040"),
            Some(("127.0.0.1".to_string(), 4040))
        );
        assert_eq!(parse_connect_target("chatgpt.com"), None); // no port
        assert_eq!(parse_connect_target("chatgpt.com:https"), None); // non-numeric
        assert_eq!(parse_connect_target(":443"), None); // empty host
    }

    #[test]
    fn parse_connect_target_rejects_non_hostname_charset() {
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
        assert_eq!(
            gate("127.0.0.1:4040#.chatgpt.com:443"),
            Gate::Deny("unparseable CONNECT authority")
        );
    }

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
        assert_eq!(gate("127.0.0.1:4040"), Gate::Deny("port != 443"));
        assert_eq!(gate("127.0.0.1:4041"), Gate::Deny("port != 443"));
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
        assert_eq!(
            gate("auth.openai.com.:443"),
            Gate::Allow("auth.openai.com".to_string())
        );
        assert_eq!(
            gate("chatgpt.com.:443"),
            Gate::Allow("chatgpt.com".to_string())
        );
        assert_eq!(
            gate("ab.chatgpt.com.:443"),
            Gate::Allow("ab.chatgpt.com".to_string())
        );
    }

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
        let head2 = b"connect a.b:443 HTTP/1.1\r\n\r\n";
        assert_eq!(connect_target_from_head(head2).as_deref(), Some("a.b:443"));
        let get = b"GET http://x/ HTTP/1.1\r\n\r\n";
        assert_eq!(connect_target_from_head(get), None);
    }

    #[test]
    fn connect_target_requires_exactly_three_tokens_and_known_version() {
        assert_eq!(
            connect_target_from_head(b"CONNECT a:443 HTTP/1.1\r\n\r\n").as_deref(),
            Some("a:443")
        );
        assert_eq!(
            connect_target_from_head(b"CONNECT a:443 HTTP/1.0\r\n\r\n").as_deref(),
            Some("a:443")
        );
        assert_eq!(connect_target_from_head(b"CONNECT a:443\r\n\r\n"), None);
        assert_eq!(
            connect_target_from_head(b"CONNECT a:443 HTTP/1.1 extra\r\n\r\n"),
            None
        );
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
        let mut cur = Cursor::new(b"CONNECT a:443 HTTP/1.1\r\n\r\nTUNNEL".to_vec());
        let (head, leftover) = read_http_head(&mut cur).await.unwrap();
        assert!(head.ends_with(b"\r\n\r\n"));
        assert_eq!(&leftover, b"TUNNEL");
    }

    #[tokio::test]
    async fn read_http_head_accepts_head_exactly_at_cap() {
        use std::io::Cursor;
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
        let mut data = vec![b'a'; 17 * 1024];
        data.extend_from_slice(b"\r\n\r\n");
        let mut cur = Cursor::new(data);
        let err = read_http_head(&mut cur).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn read_http_head_rejects_terminator_straddling_cap() {
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

        let mut chunks = VecDeque::new();
        chunks.push_back(vec![b'a'; MAX_HEAD_BYTES - 1]);
        chunks.push_back(b"aa\r\n\r\n".to_vec());
        let mut reader = Scripted(chunks);
        let err = read_http_head(&mut reader).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn serve_client_times_out_silent_client_with_408() {
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let _accepted = listener.accept().await; // hold it, never respond
            std::future::pending::<()>().await;
        });
        let (client, mut peer) = tokio::io::duplex(4096);
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
