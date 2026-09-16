//! Real host entry point; all children, sockets and state are isolated fixtures.
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.0.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
fn executable(path: &std::path::Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn local_ready(address: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(100)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
    if stream
        .write_all(b"GET / HTTP/1.1\r\nHost: fixture\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut body = String::new();
    stream.read_to_string(&mut body).is_ok() && body.contains("fixture-local-ready")
}

#[test]
fn tailnet_operational_startup_failure_preserves_local_service() {
    for fault in [
        "future-schema",
        "corrupt-state",
        "socket-directory",
        "state-file",
    ] {
        let temp = tempfile::Builder::new().prefix("nt-").tempdir().unwrap();
        let root = temp.path();
        let data = root.join("data");
        fs::create_dir(&data).unwrap();
        // Adoption is a production-supported path; the fixture's Unix listener
        // only provides readiness and never implements the host under test.
        let _proc = UnixListener::bind(data.join("proc-supervisor.sock")).unwrap();
        let private = data.join("tailnet");
        let bytes = match fault {
            "future-schema" => {
                b"{\"schemaVersion\":2,\"configRevision\":1,\"desiredEnabled\":false}".as_slice()
            }
            "corrupt-state" => b"{broken",
            _ => b"{\"schemaVersion\":1,\"configRevision\":1,\"desiredEnabled\":false}",
        };
        if fault == "state-file" {
            fs::write(&private, b"leave-this-file").unwrap();
        } else {
            fs::create_dir(&private).unwrap();
            fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(private.join("desired.json"), bytes).unwrap();
            if fault == "socket-directory" {
                fs::create_dir(private.join("app.sock")).unwrap();
            }
        }
        let kernel = root.join("kernel.py");
        let helper = root.join("helper.py");
        executable(
            &kernel,
            &format!(
                r#"#!/usr/bin/python3
import http.server,json,os,pathlib,socketserver,sys
if '--version' in sys.argv: print('fixture-kernel 0.1.0');sys.exit(0)
pathlib.Path({args}).write_text(json.dumps(sys.argv[1:]))
class H(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_GET(self):
  self.send_response(200);self.end_headers();self.wfile.write(b'fixture-local-ready')
host,port=os.environ['CALM_LISTEN'].rsplit(':',1)
socketserver.TCPServer((host,int(port)),H).serve_forever()
"#,
                args = serde_json::to_string(&root.join("kernel-args.json")).unwrap()
            ),
        );
        executable(
            &helper,
            &format!(
                r#"#!/usr/bin/python3
import pathlib,sys
if '--version' in sys.argv: print('fixture-helper 0.1.0');sys.exit(0)
pathlib.Path({marker}).write_text('unexpected helper startup')
"#,
                marker = serde_json::to_string(&root.join("helper-started")).unwrap()
            ),
        );
        let admin = address();
        let local = address();
        let config = root.join("config.toml");
        fs::write(
            &config,
            format!(
                r#"[admin]
listen = "{admin}"
token_file = ""
[child]
bin = "{kernel}"
proc_supervisor_bin = "/bin/false"
data_dir = "{data}"
calm_listen = "{local}"
db_url = "mock"
[tailnet]
provider = "private-tailnet"
binary = "{helper}"
state_dir = "{private}"
[timing]
restart_delay_ms = 20
stop_grace_ms = 100
"#,
                kernel = kernel.display(),
                helper = helper.display(),
                data = data.display(),
                private = private.display()
            ),
        )
        .unwrap();
        let mut host = Host(
            Command::new(env!("CARGO_BIN_EXE_neige-app"))
                .args(["system", "serve", "--config"])
                .arg(&config)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut ready = false;
        while Instant::now() < deadline {
            if local_ready(local) {
                ready = true;
                break;
            }
            if host.0.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            ready,
            "{fault}: optional Tailnet startup failure stopped the local kernel"
        );
        assert!(
            !root.join("helper-started").exists(),
            "{fault}: failure must not guess enabled state"
        );
        let args: Vec<String> =
            serde_json::from_slice(&fs::read(root.join("kernel-args.json")).unwrap()).unwrap();
        assert!(
            args.iter()
                .any(|arg| arg == "--private-tailnet-unavailable"),
            "{fault}: Settings must receive an explicit failure, not a fabricated disabled state"
        );
        assert!(!args.iter().any(|arg| arg == "--private-tailnet-config"));
        if fault == "state-file" {
            assert_eq!(fs::read(&private).unwrap(), b"leave-this-file");
        } else {
            assert_eq!(fs::read(private.join("desired.json")).unwrap(), bytes);
        }
    }
}
