//! Local protocol server for production quote requests, fabricated data only.
use futures_util::{SinkExt, StreamExt};
use longbridge_proto::{control, quote};
use prost::Message;

use std::sync::{Arc, Mutex};
#[derive(Clone, Debug)]
pub struct Arrival {
    pub command: u8,
    pub at: std::time::Instant,
}
pub struct Server {
    pub url: String,
    pub arrivals: Arc<Mutex<Vec<Arrival>>>,
}
pub fn start() -> String {
    start_with_gate(None).url
}
pub fn start_with_gate(gate: Option<(u8, Arc<tokio::sync::Semaphore>)>) -> Server {
    let arrivals = Arc::new(Mutex::new(Vec::new()));
    let log = arrivals.clone();
    let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    // A dedicated thread/runtime also serves process-level tests while their
    // parent runtime is blocked waiting for the child. No credentials are logged.
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            while let Ok((stream, _)) = listener.accept().await {
                let (log, gate, first) = (log.clone(), gate.clone(), first.clone());
                tokio::spawn(async move {
                    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(message)) = socket.next().await {
                        let tokio_tungstenite::tungstenite::Message::Binary(request) = message
                        else {
                            continue;
                        };
                        if request.len() < 11 || request[0] & 15 != 1 {
                            continue;
                        }
                        let command = request[1];
                        log.lock().unwrap().push(Arrival {
                            command,
                            at: std::time::Instant::now(),
                        });
                        if let Some((held, gate)) = &gate
                            && *held == command
                            && first.swap(false, std::sync::atomic::Ordering::SeqCst)
                        {
                            let Ok(permit) = gate.acquire().await else {
                                return;
                            };
                            permit.forget();
                        }
                        let len =
                            u32::from_be_bytes([0, request[8], request[9], request[10]]) as usize;
                        if request.len() < 11 + len {
                            return;
                        }
                        let body = &request[11..11 + len];
                        let body = match command {
                            2 => control::AuthResponse {
                                session_id: "fixture-session".into(),
                                expires: i64::MAX,
                                ..Default::default()
                            }
                            .encode_to_vec(),
                            4 => quote::UserQuoteProfileResponse {
                                quote_level: "fixture".into(),
                                ..Default::default()
                            }
                            .encode_to_vec(),
                            27 => {
                                let q =
                                    quote::SecurityHistoryCandlestickRequest::decode(body).unwrap();
                                assert_eq!(q.period, 1000);
                                assert_eq!(q.adjust_type, 0);
                                assert_eq!(q.trade_session, 0);
                                let end = q.date_request.unwrap().end_date;
                                let d = chrono::NaiveDate::parse_from_str(&end, "%Y%m%d").unwrap();
                                let timestamp =
                                    d.and_hms_opt(21, 0, 0).unwrap().and_utc().timestamp();
                                quote::SecurityCandlestickResponse {
                                    symbol: q.symbol,
                                    candlesticks: vec![quote::Candlestick {
                                        open: "10".into(),
                                        close: "11.123456789".into(),
                                        low: "9".into(),
                                        high: "12".into(),
                                        volume: 100,
                                        turnover: "1100.123456789".into(),
                                        timestamp,
                                        trade_session: 0,
                                    }],
                                }
                                .encode_to_vec()
                            }
                            _ => Vec::new(),
                        };
                        let mut response = vec![2, command];
                        response.extend_from_slice(&request[2..6]);
                        response.push(0);
                        let len = (body.len() as u32).to_be_bytes();
                        response.extend_from_slice(&len[1..]);
                        response.extend(body);
                        if socket
                            .send(tokio_tungstenite::tungstenite::Message::Binary(
                                response.into(),
                            ))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
    });
    Server {
        url: format!("ws://{address}/v2"),
        arrivals,
    }
}
