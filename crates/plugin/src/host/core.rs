//! core responsibilities of the plugin host.
use super::*;

impl<E: ErrorFactory> PluginHost<E> {
    pub fn new_full(inputs: HostInputs<E>) -> Self {
        let HostInputs {
            registry,
            store: repo,
            plugins_dir,
            plugins_data_dir,
            plugins_disabled,
            callbacks,
            state_sink,
            backends,
            kernel_version,
        } = inputs;
        let lifecycle_db = Arc::new(lifecycle::RepoLifecycleDb::new(repo.clone()));
        let plugin_list_db = Arc::new(RepoPluginListDb { repo: repo.clone() });
        Self {
            registry,
            repo,
            plugin_list_db,
            plugin_list_wall: PLUGIN_LIST_WALL,
            plugins_dir,
            plugins_data_dir,
            plugins_disabled,
            callbacks,
            state_sink,
            backends,
            kernel_version,
            processes: std::sync::Mutex::new(ProcessTable::default()),
            spawn_order: std::sync::Mutex::new(HashMap::new()),
            config_gate: std::sync::Mutex::new(std::collections::HashSet::new()),
            config_gate_breaches: std::sync::Mutex::new(HashMap::new()),
            lifecycle: std::sync::Mutex::new(HashMap::new()),
            run_epoch_seq: std::sync::atomic::AtomicU64::new(1),
            lifecycle_db,
            app_autospawn_wall: APP_AUTOSPAWN_WALL,
            backoff: BackoffConfig::default(),
            #[cfg(feature = "test-support")]
            supervisor_handshake: std::sync::Mutex::new(None),
        }
    }
    /// Post-construction override of the narrow `LifecycleDb` port; production never calls this.
    #[must_use]
    pub fn with_lifecycle_db(mut self, db: Arc<dyn lifecycle::LifecycleDb<E>>) -> Self {
        self.lifecycle_db = db;
        self
    }

    /// Post-construction override of the plugin-list read port; production never calls this.
    /// Overrides enumeration only; later reads and writes still use `self.repo`.
    #[must_use]
    pub fn with_plugin_list_db(mut self, db: Arc<dyn PluginListDb<E>>) -> Self {
        self.plugin_list_db = db;
        self
    }

    /// Post-construction override of `PLUGIN_LIST_WALL`; production never calls this.
    #[must_use]
    pub fn with_plugin_list_wall(mut self, wall: Duration) -> Self {
        self.plugin_list_wall = wall;
        self
    }

    /// Post-construction override of the crash-window / respawn backoff tunables. `schedule_ms` must be non-empty.
    #[must_use]
    pub fn with_backoff_schedule(
        mut self,
        schedule_ms: Vec<u64>,
        crash_window: Duration,
        crash_window_limit: u32,
    ) -> Self {
        assert!(
            !schedule_ms.is_empty(),
            "backoff schedule must have at least one entry"
        );
        self.backoff = BackoffConfig {
            schedule_ms,
            crash_window,
            crash_window_limit,
        };
        self
    }

    /// Post-construction override of `APP_AUTOSPAWN_WALL`.
    #[must_use]
    pub fn with_app_autospawn_wall(mut self, wall: Duration) -> Self {
        self.app_autospawn_wall = wall;
        self
    }

    /// Read-only view of the registry: the mutators are module-private and take a `LifecycleGuard`, so this handle grants no write capability outside `plugin_host`.
    pub fn registry(&self) -> &Arc<PluginRegistry> {
        &self.registry
    }

    /// The `Arc<Mutex>` for `id`, creating the map entry on first use.
    pub(super) fn lifecycle_cell(&self, id: &str) -> Arc<Mutex<()>> {
        let mut map = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            &map.entry(id.to_string())
                .or_insert_with(|| {
                    Arc::new(LifecycleCell {
                        lock: Arc::new(Mutex::new(())),
                    })
                })
                .lock,
        )
    }

    /// **External** acquisition: non-blocking, returns `LifecycleBusy` having done nothing, so the refusal is safely retryable.
    pub fn try_lock_lifecycle(&self, id: &str) -> Result<LifecycleGuard, HostError> {
        let cell = self.lifecycle_cell(id);
        match cell.try_lock_owned() {
            Ok(held) => Ok(LifecycleGuard {
                id: id.to_string(),
                _held: held,
            }),
            Err(_) => Err(HostError::LifecycleBusy(id.to_string())),
        }
    }

    /// **Internal** acquisition: waits. For callers with nobody to answer (crash supervisor, boot reconciliation); each must re-decide everything afterwards. No wait budget on purpose.
    pub(super) async fn await_lifecycle(&self, id: &str) -> LifecycleGuard {
        let cell = self.lifecycle_cell(id);
        let held = cell.lock_owned().await;
        LifecycleGuard {
            id: id.to_string(),
            _held: held,
        }
    }

    /// Allocate the next [`RunningPlugin::run_epoch`].
    pub(super) fn next_run_epoch(&self) -> u64 {
        self.run_epoch_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Runtime registry write, routed through the host so it requires the lifecycle guard.
    pub fn registry_insert(
        &self,
        guard: &LifecycleGuard,
        manifest: manifest::Manifest,
        install_path: Option<PathBuf>,
    ) {
        self.registry.insert(guard, manifest, install_path);
    }

    /// Runtime registry removal.
    pub fn registry_remove(&self, guard: &LifecycleGuard) -> Option<manifest::Manifest> {
        self.registry.remove(guard)
    }

    /// Lock the process table, recovering from poison so `AdmissionGuard::drop` can release during a panic unwind.
    pub(super) fn lock_table(&self) -> std::sync::MutexGuard<'_, ProcessTable> {
        self.processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
