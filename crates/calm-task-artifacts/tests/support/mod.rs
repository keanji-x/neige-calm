#![allow(dead_code)]
use calm_task_artifacts::{
    ArtifactStore, CaptureReceipt, CaptureRequest, GitConfig, Limits, OutputSlot, QuiescentSource,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use tempfile::TempDir;

pub fn limits() -> Limits {
    Limits {
        max_entries: 256,
        max_file_bytes: 1024 * 1024,
        max_total_bytes: 4 * 1024 * 1024,
        max_manifest_bytes: 1024 * 1024,
        max_path_bytes: 512,
        max_depth: 32,
    }
}
pub fn git_config() -> GitConfig {
    GitConfig {
        binary: "/usr/bin/git".into(),
        timeout: Duration::from_secs(5),
    }
}
pub fn git(source: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new("/usr/bin/git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .arg("-C")
        .arg(source)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
pub fn source(path: &Path) {
    fs::create_dir(path).unwrap();
    git(path, &["init", "--quiet"]);
}
pub fn slot(name: &str, paths: &[&str]) -> OutputSlot {
    OutputSlot {
        name: name.into(),
        paths: paths.iter().map(|s| (*s).into()).collect(),
    }
}
pub struct Rig {
    pub temp: TempDir,
    pub source: PathBuf,
    pub root: PathBuf,
    pub store: ArtifactStore,
}
impl Rig {
    pub fn new() -> Self {
        let temp = TempDir::new().unwrap();
        let source_path = temp.path().join("source");
        source(&source_path);
        let root = temp.path().join("store");
        let store = ArtifactStore::open(&root, limits(), git_config()).unwrap();
        Self {
            temp,
            source: source_path,
            root,
            store,
        }
    }
    pub fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.source.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    pub fn capture(&self, key: &str, outputs: &[OutputSlot]) -> CaptureReceipt {
        self.store
            .capture(CaptureRequest {
                key,
                source: QuiescentSource {
                    root: &self.source,
                    boundary_id: "test-stopped-runtime-1",
                },
                outputs,
            })
            .unwrap()
    }
    pub fn destination(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }
}
