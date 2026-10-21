//! Owned, cancellation-safe quote protocol v1 transport (command 27 only).
//!
//! QuoteContext at the pinned SDK revision queues detached requests after their
//! caller is cancelled. This small adapter uses its official protobuf messages
//! and SDK OAuth/HTTP bootstrap, but owns the socket and every actual write.
//! No background task, reconnect loop, retry, or command queue may outlive a call.
use super::{Fetched, LongbridgeSource, failed, invalid_body};
use crate::types::{ErrorClass, FailureClass};
use futures_util::{SinkExt, StreamExt};
use longbridge::httpclient::{DC_REGION_HEADER, DcRegion};
use longbridge_proto::{Error as ApiError, control};
use prost::Message as ProtoMessage;
use std::io::Read;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};
const MAX_BODY: usize = 8 * 1024 * 1024;

pub(super) struct QuoteWire {
    socket: WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>,
    next_id: u32,
    expires: i64,
}
impl QuoteWire {
    pub(super) fn valid(&self) -> bool {
        self.expires > chrono::Utc::now().timestamp_millis() + 1000
    }
    pub(super) async fn open(
        source: &LongbridgeSource,
        url: &str,
        token: &str,
        may_send: &(impl Fn() -> bool + Sync),
    ) -> Result<Option<Self>, Fetched> {
        let http = source
            .snapshot_for(token.to_owned())
            .await
            .map_err(|e| super::candles::auth_failure(crate::sdk_error::oauth_kind(&e)))?;
        source.limiter.acquire().await;
        if !may_send() {
            return Ok(None);
        }
        // Keep the SDK's authoritative OTP response and connection-limit checks.
        let otp = http.get_otp().await.map_err(super::classify)?;
        let mut url = url::Url::parse(url).map_err(|_| invalid_body("invalid_quote_url"))?;
        url.query_pairs_mut()
            .extend_pairs([("version", "1"), ("codec", "1"), ("platform", "9")]);
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| invalid_body("invalid_quote_url"))?;
        request
            .headers_mut()
            .insert("accept-language", "en".parse().expect("fixed header"));
        let region = DcRegion::from_credentials(&[token]);
        request.headers_mut().insert(
            DC_REGION_HEADER,
            region.as_str().parse().expect("SDK region"),
        );
        source.limiter.acquire().await;
        if !may_send() {
            return Ok(None);
        }
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_BODY + 34))
            .max_frame_size(Some(MAX_BODY + 34));
        let (socket, _) =
            tokio_tungstenite::connect_async_with_config(request, Some(config), false)
                .await
                .map_err(|_| transport("quote_connect"))?;
        let mut wire = Self {
            socket,
            next_id: 1,
            expires: i64::MAX,
        };
        let auth = control::AuthRequest {
            token: otp,
            metadata: std::collections::HashMap::from([("accept-language".into(), "en".into())]),
        };
        let Some(body) = wire.call(2, &auth, source, may_send).await? else {
            return Ok(None);
        };
        let auth = control::AuthResponse::decode(body.as_slice())
            .map_err(|_| invalid_body("invalid_quote_auth"))?;
        if auth.session_id.is_empty() || auth.expires <= chrono::Utc::now().timestamp_millis() {
            return Err(invalid_body("invalid_quote_auth"));
        }
        wire.expires = auth.expires;
        Ok(Some(wire))
    }

    pub(super) async fn call(
        &mut self,
        command: u8,
        body: &impl ProtoMessage,
        source: &LongbridgeSource,
        may_send: &(impl Fn() -> bool + Sync),
    ) -> Result<Option<Vec<u8>>, Fetched> {
        let id = self.next_id;
        self.next_id = id
            .checked_add(1)
            .ok_or_else(|| invalid_body("quote_request_id_exhausted"))?;
        let body = body.encode_to_vec();
        if body.len() > MAX_BODY {
            return Err(invalid_body("quote_request_too_large"));
        }
        let mut packet = vec![1, command];
        packet.extend(id.to_be_bytes());
        packet.extend(30_000_u16.to_be_bytes());
        packet.extend(&(body.len() as u32).to_be_bytes()[1..]);
        packet.extend(body);
        source.limiter.acquire().await;
        if !may_send() {
            return Ok(None);
        }
        self.socket
            .send(Message::Binary(packet.into()))
            .await
            .map_err(|_| transport("quote_send"))?;
        loop {
            match self.socket.next().await {
                Some(Ok(Message::Binary(packet))) => {
                    if let Some(body) = decode(&packet, command, id)? {
                        return Ok(Some(body));
                    }
                }
                Some(Ok(Message::Ping(_))) => {
                    // Tungstenite queued its automatic pong while reading.
                    // Fence and pace its flush instead of allowing the next read
                    // to send this control write outside the archive boundary.
                    source.limiter.acquire().await;
                    if !may_send() {
                        return Ok(None);
                    }
                    self.socket
                        .flush()
                        .await
                        .map_err(|_| transport("quote_pong"))?;
                }
                Some(Ok(Message::Pong(_))) => {}
                _ => return Err(transport("quote_closed")),
            }
        }
    }
}
fn transport(code: &str) -> Fetched {
    Fetched {
        response: failed(
            FailureClass::Transient,
            ErrorClass::Transport,
            code.to_owned(),
        ),
        retry_after: None,
    }
}

fn decode(packet: &[u8], command: u8, id: u32) -> Result<Option<Vec<u8>>, Fetched> {
    if packet.is_empty() || packet[0] & 0xc0 != 0 {
        return Err(invalid_body("invalid_quote_packet"));
    }
    // No subscriptions are made. Ignore bounded pushes; the enclosing whole
    // operation timeout also bounds continuous unrelated frames.
    if packet[0] & 15 == 3 {
        return Ok(None);
    }
    if packet.len() < 10
        || packet[0] & 15 != 2
        || packet[1] != command
        || packet[2..6] != id.to_be_bytes()
    {
        return Err(invalid_body("invalid_quote_correlation"));
    }
    let len = u32::from_be_bytes([0, packet[7], packet[8], packet[9]]) as usize;
    let signature = if packet[0] & 0x10 != 0 { 24 } else { 0 };
    if len > MAX_BODY || packet.len() != 10 + len + signature {
        return Err(invalid_body("invalid_quote_length"));
    }
    let payload = &packet[10..10 + len];
    let body = if packet[0] & 0x20 != 0 {
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(payload)
            .take((MAX_BODY + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid_body("invalid_quote_gzip"))?;
        if bytes.len() > MAX_BODY {
            return Err(invalid_body("quote_inflated_too_large"));
        }
        bytes
    } else {
        payload.to_vec()
    };
    if packet[6] != 0 {
        let error =
            ApiError::decode(body.as_slice()).map_err(|_| invalid_body("invalid_quote_error"))?;
        let code =
            i64::try_from(error.code).map_err(|_| invalid_body("invalid_quote_error_code"))?;
        return Err(super::candles::quote_code(code));
    }
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Response;
    fn packet(body: &[u8], flags: u8, status: u8) -> Vec<u8> {
        let mut p = vec![2 | flags, 27, 0, 0, 0, 1, status];
        p.extend(&(body.len() as u32).to_be_bytes()[1..]);
        p.extend(body);
        p
    }
    #[test]
    fn correlation_lengths_compression_and_error_codes_are_checked() {
        let body = b"protobuf-payload";
        let p = packet(body, 0, 0);
        assert_eq!(decode(&p, 27, 1).unwrap().unwrap(), body);
        assert!(decode(&p, 27, 2).is_err());
        assert!(decode(&p, 4, 1).is_err());
        let mut short = p.clone();
        short.pop();
        assert!(decode(&short, 27, 1).is_err());
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gzip, body).unwrap();
        let gzip = gzip.finish().unwrap();
        assert_eq!(
            decode(&packet(&gzip, 0x20, 0), 27, 1).unwrap().unwrap(),
            body
        );
        let err = ApiError {
            code: 301607,
            msg: "UNTRUSTED_CANARY_SECRET".into(),
        }
        .encode_to_vec();
        let result = decode(&packet(&err, 0, 1), 27, 1).unwrap_err();
        let Response::Failed { error, .. } = result.response() else {
            panic!("failure expected")
        };
        assert_eq!(error.code, "301607");
        assert!(!format!("{result:?}").contains("UNTRUSTED"));
    }
}
