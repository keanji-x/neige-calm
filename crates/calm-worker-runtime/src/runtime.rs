use crate::linux::{self, Observation};
use crate::storage::{self, Lock, Phase, Record};
use crate::{
    BoundaryHandle, BoundaryState, Error, LaunchConfig, NetworkPolicy, QuiescenceProof, Result,
    Runtime, RuntimeConfig,
};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

impl Runtime {
    /// Blocking API; async callers should use their blocking executor.
    pub fn new(mut config: RuntimeConfig) -> Result<Self> {
        if config.timeout.is_zero() || config.timeout > Duration::from_secs(60) {
            return Err(Error::Conflict("timeout must be within (0, 60s]".into()));
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&config.state_root)?;
        config.state_root = config.state_root.canonicalize()?;
        if config.state_root.starts_with("/usr") {
            return Err(Error::Conflict(
                "state root cannot be inside a system mount".into(),
            ));
        }
        let metadata = std::fs::metadata(&config.state_root)?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(Error::Conflict(
                "state root must be owned by caller with mode 0700".into(),
            ));
        }
        config.helper = config.helper.canonicalize()?;
        config.bwrap = config.bwrap.canonicalize()?;
        Ok(Self { config })
    }

    /// Executes only the trusted helper, never provider code or network calls.
    pub fn preflight(&self, network: NetworkPolicy) -> Result<()> {
        let pid = i32::try_from(std::process::id())
            .map_err(|_| Error::Unsupported("PID range".into()))?;
        linux::PidFd::open(pid)
            .and_then(|fd| fd.signal(0))
            .map_err(|error| Error::Unsupported(format!("pidfd capability: {error}")))?;
        let policy = match network {
            NetworkPolicy::Provider => "provider",
            NetworkPolicy::Isolated => "isolated",
        };
        let parent_network = std::fs::metadata("/proc/self/ns/net")?.ino().to_string();
        let mut child = linux::base_command(&self.config.bwrap, &self.config.helper, network)
            .args(["--", "/boundary-helper", "check", policy, &parent_network])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| Error::Unsupported(format!("bwrap launch: {error}")))?;
        let until = Instant::now() + self.config.timeout;
        loop {
            if let Some(status) = child.try_wait()? {
                if status.success() {
                    return Ok(());
                }
                let mut message = String::new();
                if let Some(stderr) = child.stderr.take() {
                    stderr.take(8192).read_to_string(&mut message)?;
                }
                return Err(Error::Unsupported(format!(
                    "namespace preflight {status}: {message}"
                )));
            }
            if Instant::now() >= until {
                child.kill()?;
                child.wait()?;
                return Err(Error::Unsupported("namespace preflight timed out".into()));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Reserves run identity durably. An existing run is never relaunched.
    /// Persist the returned handle in the caller's transaction before start.
    pub fn prepare(&self, run_id: &str, launch_config: &LaunchConfig) -> Result<BoundaryHandle> {
        validate_run_id(run_id)?;
        // Submitted identity is independent of filesystem availability and of
        // the canonical paths chosen once for the first launch.
        let request_fingerprint =
            format!("{:x}", Sha256::digest(serde_json::to_vec(launch_config)?));
        let directory = self.directory(run_id)?;
        {
            let _lock = Lock::acquire(&self.config.state_root)?;
            if directory.try_exists()? {
                let record = storage::read(&directory)?;
                if record.request_fingerprint != request_fingerprint || record.run_id != run_id {
                    return Err(Error::Conflict(
                        "run ID already names a different launch".into(),
                    ));
                }
                if let Some(handle) = record.handle() {
                    record.check_handle(handle)?;
                    return Ok(handle.clone());
                }
            } else {
                let launch_config = self.validate_launch_config(launch_config)?;
                let config_digest =
                    format!("{:x}", Sha256::digest(serde_json::to_vec(&launch_config)?));
                self.preflight(launch_config.network)?;
                std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
                File::open(&self.config.state_root)?.sync_all()?;
                let record = Record {
                    version: storage::RECORD_VERSION,
                    run_id: run_id.into(),
                    request_fingerprint: request_fingerprint.clone(),
                    launch_config,
                    config_digest,
                    helper: self.config.helper.clone(),
                    bwrap: self.config.bwrap.clone(),
                    token: uuid::Uuid::new_v4().to_string(),
                    phase: Phase::Reserved,
                };
                storage::write(&directory, &record)?;
                let stderr = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(directory.join("launcher.log"))?;
                let mut child = Command::new(&self.config.helper)
                    .env_clear()
                    .process_group(0)
                    .arg("launch")
                    .arg(&directory)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(stderr)
                    .spawn()?;
                // The launcher survives caller exit. It owns and reaps its bwrap child.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
        }
        let until = Instant::now() + self.config.timeout;
        loop {
            let record = storage::read(&directory)?;
            if record.request_fingerprint != request_fingerprint || record.run_id != run_id {
                return Err(Error::Conflict(
                    "run ID already names a different launch".into(),
                ));
            }
            if let Some(handle) = record.handle() {
                record.check_handle(handle)?;
                return Ok(handle.clone());
            }
            if Instant::now() >= until {
                return Err(Error::Evidence(
                    "preparation has no captured init identity; run not relaunched".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Idempotently grants one execution. Does not assert provider RPC readiness.
    pub fn start(&self, handle: &BoundaryHandle) -> Result<()> {
        let directory = self.directory(&handle.run_id)?;
        let _lock = Lock::acquire(&directory)?;
        let mut record = storage::read(&directory)?;
        record.check_handle(handle)?;
        match &record.phase {
            Phase::Prepared { launcher_pid, .. } => {
                if !matches!(linux::observe(&handle.init), Observation::Live(_)) {
                    return Err(Error::Evidence("prepared init is not proven live".into()));
                }
                record.phase = Phase::Running {
                    handle: handle.clone(),
                    launcher_pid: *launcher_pid,
                };
                storage::write(&directory, &record)
            }
            Phase::Running { .. } => Ok(()),
            _ => Err(Error::Conflict("run is closed to start".into())),
        }
    }

    /// Raw provider stdin/stdout. One live reader is admitted at a time. No
    /// synthetic protocol frames, automatic provider restart, or transcript replay.
    pub fn connect_stdio(&self, handle: &BoundaryHandle) -> Result<UnixStream> {
        let directory = self.directory(&handle.run_id)?;
        let record = storage::read(&directory)?;
        record.check_handle(handle)?;
        if !matches!(record.phase, Phase::Prepared { .. } | Phase::Running { .. }) {
            return Err(Error::Conflict("stdio unavailable for closed run".into()));
        }
        let (_fd, path) = linux::socket_path(&directory)?;
        Ok(UnixStream::connect(path)?)
    }

    /// Evidence loss remains Unknown; wrapper exit is never a substitute.
    pub fn probe(&self, handle: &BoundaryHandle) -> Result<BoundaryState> {
        let result = || -> Result<BoundaryState> {
            let directory = self.directory(&handle.run_id)?;
            let _lock = Lock::acquire(&directory)?;
            let mut record = storage::read(&directory)?;
            record.check_handle(handle)?;
            if let Phase::Closed(proof) = &record.phase {
                return Ok(BoundaryState::Quiesced(proof.clone()));
            }
            match linux::observe(&handle.init) {
                Observation::Gone(method) => {
                    let proof = QuiescenceProof {
                        handle: handle.clone(),
                        observed_at_ms: storage::now_ms(),
                        method: method.into(),
                    };
                    record.phase = Phase::Closed(proof.clone());
                    storage::write(&directory, &record)?;
                    Ok(BoundaryState::Quiesced(proof))
                }
                Observation::Unknown(reason) => Ok(BoundaryState::Unknown(reason)),
                Observation::Live(_) => Ok(match record.phase {
                    Phase::Prepared { .. } => BoundaryState::Prepared,
                    Phase::Running { .. } => BoundaryState::Running,
                    _ => BoundaryState::Unknown("stopping; init exit not yet proven".into()),
                }),
            }
        };
        Ok(result().unwrap_or_else(|error| BoundaryState::Unknown(error.to_string())))
    }

    /// Durably closes start admission, kills only the pinned namespace init and
    /// waits for evidence. A timeout returns Unknown, never a fabricated proof.
    pub fn stop(&self, handle: &BoundaryHandle, deadline: Duration) -> Result<BoundaryState> {
        if deadline > Duration::from_secs(60) {
            return Err(Error::Conflict("stop deadline exceeds 60s".into()));
        }
        let directory = self.directory(&handle.run_id)?;
        {
            let _lock = Lock::acquire(&directory)?;
            let mut record = storage::read(&directory)?;
            record.check_handle(handle)?;
            match record.phase {
                Phase::Prepared { launcher_pid, .. } | Phase::Running { launcher_pid, .. } => {
                    record.phase = Phase::Closing {
                        handle: handle.clone(),
                        launcher_pid,
                    };
                    storage::write(&directory, &record)?;
                }
                Phase::Closed(proof) => return Ok(BoundaryState::Quiesced(proof)),
                _ => {}
            }
        }
        match linux::observe(&handle.init) {
            Observation::Live(pidfd) => pidfd.kill()?,
            Observation::Unknown(reason) => return Ok(BoundaryState::Unknown(reason)),
            Observation::Gone(_) => {}
        }
        let until = Instant::now() + deadline;
        loop {
            let state = self.probe(handle)?;
            if matches!(state, BoundaryState::Quiesced(_)) || Instant::now() >= until {
                return Ok(state);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn directory(&self, run_id: &str) -> Result<PathBuf> {
        validate_run_id(run_id)?;
        let directory = self.config.state_root.join(run_id);
        if std::fs::symlink_metadata(&directory).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Error::Evidence("run directory cannot be a symlink".into()));
        }
        Ok(directory)
    }

    fn validate_launch_config(&self, original: &LaunchConfig) -> Result<LaunchConfig> {
        let mut config = original.clone();
        if config.attempt_id.is_empty() {
            return Err(Error::Conflict("attempt identity required".into()));
        }
        config.workspace = config.workspace.canonicalize()?;
        if !config.workspace.is_dir() {
            return Err(Error::Conflict("workspace must be a directory".into()));
        }
        self.check_source(&config.workspace)?;
        absolute_normal(&config.program)?;
        for (name, value) in &config.environment {
            if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
                return Err(Error::Conflict("invalid explicit environment".into()));
            }
        }
        let reserved = [
            "/usr",
            "/bin",
            "/sbin",
            "/lib",
            "/lib64",
            "/proc",
            "/dev",
            "/tmp",
            "/workspace",
            "/boundary-helper",
        ];
        let mut destinations: Vec<PathBuf> = reserved.iter().map(PathBuf::from).collect();
        for mount in &mut config.mounts {
            mount.source = mount.source.canonicalize()?;
            self.check_source(&mount.source)?;
            absolute_normal(&mount.destination)?;
            if destinations.iter().any(|path| {
                path.starts_with(&mount.destination) || mount.destination.starts_with(path)
            }) {
                return Err(Error::Conflict(
                    "overlapping or reserved mount destination".into(),
                ));
            }
            destinations.push(mount.destination.clone());
        }
        if serde_json::to_vec(&config)?.len() > 65_536 {
            return Err(Error::Conflict(
                "launch configuration exceeds 64 KiB".into(),
            ));
        }
        Ok(config)
    }

    fn check_source(&self, source: &Path) -> Result<()> {
        if source.starts_with(&self.config.state_root) || self.config.state_root.starts_with(source)
        {
            return Err(Error::Conflict(
                "Worker mount would expose private process evidence".into(),
            ));
        }
        Ok(())
    }
}

fn absolute_normal(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(Error::Conflict("absolute normalized path required".into()));
    }
    Ok(())
}
fn validate_run_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(Error::Conflict("invalid run ID".into()));
    }
    Ok(())
}
