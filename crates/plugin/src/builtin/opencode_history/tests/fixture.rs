use super::super::*;
use serde_json::json;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
pub(super) fn session() -> Value {
    json!({"id":"ses_allowed","slug":"allowed","projectID":"project-a","directory":"/work/allowed","title":"Allowed history","version":"1.19.3","time":{"created":1,"updated":2}})
}
pub(super) fn part() -> Value {
    serde_json::from_str(include_str!("text-part.json")).unwrap()
}
pub(super) fn message() -> Value {
    json!({"info":{"id":"msg_one","sessionID":"ses_allowed","role":"user","time":{"created":1},"agent":"build","model":{"providerID":"synthetic","modelID":"synthetic"}},"parts":[part()]})
}
#[derive(Clone)]
pub(super) struct Reply {
    pub(super) status: u16,
    pub(super) body: Value,
    pub(super) headers: Vec<(String, String)>,
    pub(super) raw: Option<String>,
    pub(super) delay: bool,
}
impl Reply {
    pub(super) fn json(v: Value) -> Self {
        Self {
            status: 200,
            body: v,
            headers: vec![],
            raw: None,
            delay: false,
        }
    }
}
pub(super) struct Fixture {
    pub(super) tmp: tempfile::TempDir,
    pub(super) config: Value,
    pub(super) values: Value,
    pub(super) replies: Arc<Mutex<BTreeMap<String, Reply>>>,
    pub(super) requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    pub(super) address: std::net::SocketAddr,
    task: Option<thread::JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        self.task.take().unwrap().join().unwrap();
    }
}
impl Fixture {
    pub(super) fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let project = json!({"id":"project-a","worktree":"/work/allowed","sandboxes":["/work/second"],"time":{"created":1,"updated":1}});
        let replies = Arc::new(Mutex::new(BTreeMap::from([
            (
                "/global/health".into(),
                Reply::json(json!({"healthy":true,"version":"1.19.3"})),
            ),
            ("/doc".into(), Reply::json(contract::DOC.clone())),
            ("/project".into(), Reply::json(json!([project.clone()]))),
            ("/project/current".into(), Reply::json(project)),
            ("/session".into(), Reply::json(json!([session()]))),
            ("/session/ses_allowed".into(), Reply::json(session())),
            (
                "/session/ses_allowed/message".into(),
                Reply::json(json!([message()])),
            ),
            (
                "/session/ses_allowed/message/msg_one".into(),
                Reply::json(message()),
            ),
        ])));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (r, q, done) = (replies.clone(), requests.clone(), stop.clone());
        let task = thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                if done.load(Ordering::SeqCst) {
                    break;
                }
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut raw = Vec::new();
                let mut buf = [0; 1024];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let raw = String::from_utf8(raw).unwrap();
                let route = raw
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .split('?')
                    .next()
                    .unwrap()
                    .to_string();
                q.lock().unwrap().push(raw);
                let reply = r
                    .lock()
                    .unwrap()
                    .get(&route)
                    .cloned()
                    .unwrap_or_else(|| Reply {
                        status: 404,
                        ..Reply::json(json!({}))
                    });
                if reply.delay {
                    thread::sleep(std::time::Duration::from_millis(100));
                }
                let body = reply.raw.unwrap_or_else(|| reply.body.to_string());
                let extra = reply
                    .headers
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}\r\n"))
                    .collect::<String>();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {} status\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                    reply.status,
                    body.len(),
                    extra,
                    body
                );
            }
        });
        let password = tmp.path().join("password");
        std::fs::write(&password, "synthetic-password").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&password, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = json!({"sources":[{"source_id":"synthetic","base_url":format!("http://{address}"),"credential_ref":"local","directories":["/work/allowed","/work/second"]}],"credentials":{"local":{"username":"opencode","password_file":password}},"limits":{"page_messages":50,"max_response_bytes":2097152,"max_result_bytes":131072,"max_parts":128,"max_part_bytes":16384,"request_timeout_ms":1000}});
        let values = json!({"history_config_path":tmp.path().join("history.json")});
        let fx = Self {
            tmp,
            config,
            values,
            replies,
            requests,
            stop,
            address,
            task: Some(task),
        };
        fx.write();
        fx
    }
    pub(super) fn write(&self) {
        std::fs::write(
            self.values["history_config_path"].as_str().unwrap(),
            self.config.to_string(),
        )
        .unwrap();
    }
    pub(super) fn load(&self) -> History {
        self.write();
        History::load(self.values.clone()).unwrap()
    }
    pub(super) fn set(&self, route: &str, body: Value) {
        self.replies
            .lock()
            .unwrap()
            .insert(route.into(), Reply::json(body));
    }
    pub(super) fn args(name: &str) -> Value {
        match name {
            "session_ls" => {
                json!({"source_id":"synthetic","directory":"/work/allowed","roots":true,"limit":2})
            }
            "message_ls" => {
                json!({"source_id":"synthetic","directory":"/work/allowed","session_id":"ses_allowed","limit":2})
            }
            "message_cat" => {
                json!({"source_id":"synthetic","directory":"/work/allowed","session_id":"ses_allowed","message_id":"msg_one"})
            }
            _ => json!({}),
        }
    }
    pub(super) fn call(&self, name: &str) -> Result<Value> {
        let r = self.load().call(name, Self::args(name))?;
        let v = serde_json::to_value(r).unwrap();
        Ok(serde_json::from_str(v["content"][0]["text"].as_str().unwrap()).unwrap())
    }
}
