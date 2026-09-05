use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[test]
fn boundary_network_policy_is_required_in_launch_configuration() {
    let f = Fixture::new();
    let mut value = serde_json::to_value(&f.config).unwrap();
    value.as_object_mut().unwrap().remove("network");
    assert!(serde_json::from_value::<LaunchConfig>(value).is_err());
}

#[test]
fn boundary_network_policy_matches_actual_namespace_without_requests() {
    let host_inode = std::fs::metadata("/proc/self/ns/net").unwrap().ino();
    for policy in [NetworkPolicy::Provider, NetworkPolicy::Isolated] {
        let mut f = Fixture::new();
        f.config.network = policy;
        f.runtime.preflight(policy).unwrap();
        let handle = f.prepare();
        let child_inode = std::fs::metadata(format!("/proc/{}/ns/net", handle.init.pid))
            .unwrap()
            .ino();
        assert_eq!(child_inode == host_inode, policy == NetworkPolicy::Provider);
        f.runtime.start(&handle).unwrap();
        wait_file(&f.config.workspace.join("beats"));
        assert_eq!(f.runtime.probe(&handle).unwrap(), BoundaryState::Running);
    }
}

#[test]
fn boundary_isolated_preflight_rejects_ignored_network_flag() {
    let f = Fixture::new();
    let wrapper = f._root.path().join("drop-network-flag");
    std::fs::write(&wrapper,
        "#!/usr/bin/python3\nimport os,sys\nargs=[a for a in sys.argv[1:] if a != '--unshare-net']\nos.execv('/usr/bin/bwrap',['/usr/bin/bwrap']+args)\n"
    ).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let runtime = Runtime::new(RuntimeConfig {
        state_root: f._root.path().join("net-state"),
        helper: env!("CARGO_BIN_EXE_calm-worker-boundary").into(),
        bwrap: wrapper,
        timeout: Duration::from_secs(4),
    })
    .unwrap();
    runtime.preflight(NetworkPolicy::Provider).unwrap();
    assert!(matches!(
        runtime.preflight(NetworkPolicy::Isolated),
        Err(calm_worker_runtime::Error::Unsupported(_))
    ));
    let mut config = f.config.clone();
    config.network = NetworkPolicy::Isolated;
    assert!(matches!(
        runtime.prepare("no-network-fallback", &config),
        Err(calm_worker_runtime::Error::Unsupported(_))
    ));
    assert!(!f.config.workspace.join("started").exists());
}
