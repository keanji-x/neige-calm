use super::config::{AcpAgentConfig, AcpPlannerConfig, AcpPlannerHost};
use calm_types::runtime::AgentProvider;
use std::time::{Duration, Instant};

#[tokio::test]
async fn c4_acp_catalog_total_deadline_kills_group() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("agent.py");
    std::fs::write(&script, r#"
import json, sys, time, subprocess, pathlib, os
root = pathlib.Path(sys.argv[1])
child = subprocess.Popen(['/bin/sleep', '300'], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
(root / 'pid').write_text(str(child.pid))
for line in sys.stdin:
    request = json.loads(line)
    time.sleep(8.5)
    method = request['method']
    if method == 'initialize':
        result = {'protocolVersion': 1, '_meta': {'agentVersion': '1.0.62'}, 'authMethods': [{'id': 'cached_token', 'name': 'token'}], 'agentCapabilities': {'sessionCapabilities': {'close': {}}}}
    elif method == 'session/new':
        result = {'sessionId': 'catalog'}
        (root / 'cwd').write_text(request['params']['cwd'])
    else:
        result = {}
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
"#).unwrap();
    let host = AcpPlannerHost::new(
        Some(AcpPlannerConfig {
            agents: vec![AcpAgentConfig {
                provider: AgentProvider::Grok,
                command: "/usr/bin/python3".into(),
                args: vec![
                    script.display().to_string(),
                    root.path().display().to_string(),
                ],
                env: Default::default(),
                min_agent_version: "1.0.62".parse().unwrap(),
                default_permission_mode: None,
            }],
        }),
        root.path(),
        "unused-shim".into(),
        root.path().join("sock"),
    )
    .unwrap();
    let started = Instant::now();
    let cwd = root.path().to_owned();
    let query = tokio::spawn(async move {
        host.discover_catalog(&AgentProvider::Grok, Some(&cwd))
            .await
    });
    let pid_file = root.path().join("pid");
    tokio::time::timeout(Duration::from_secs(5), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid: i32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
    let identity = crate::proc_identity::read_proc_start_time(pid).unwrap();
    let catalog = query.await.unwrap();
    let elapsed = started.elapsed();
    let stopped = tokio::time::timeout(Duration::from_secs(2), async {
        while crate::proc_identity::read_proc_start_time(pid).is_some()
            && std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|s| !s.split_once(") ").unwrap().1.starts_with('Z'))
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if crate::proc_identity::read_proc_start_time(pid) == Some(identity) {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("cwd")).unwrap(),
        root.path().to_str().unwrap()
    );
    assert!(stopped.is_ok(), "deadline must kill descendants");
    assert!(
        elapsed >= Duration::from_secs(29) && elapsed < Duration::from_secs(33),
        "total deadline: {elapsed:?}"
    );
    assert_eq!(
        catalog.source,
        crate::planner_model::catalog::ModelSource::Unavailable
    );
}
