//! Test transport wiring shared by unit and integration native protocol peers.
use super::*;

impl SharedCodexAppServer {
    #[doc(hidden)]
    pub async fn install_protocol_peer_for_test(
        self: &Arc<Self>,
        client: Arc<CodexAppServer>,
        notifications: crate::codex_appserver::NotificationStream,
    ) -> Result<JoinHandle<()>> {
        let forwarder = self.install_client(client.clone(), notifications).await?;
        self.publish_protocol_peer_for_test(client, ConnectionReplay::Complete)
            .await?;
        Ok(forwarder)
    }

    pub(super) async fn publish_protocol_peer_for_test(
        self: &Arc<Self>,
        client: Arc<CodexAppServer>,
        replay: ConnectionReplay,
    ) -> Result<()> {
        let launched = client.clone();
        self.start_new_process_typestate(|_| async move {
            Ok(LaunchedSharedDaemon {
                child: None,
                client: launched,
                replay,
                runtime: SharedDaemonRuntime {
                    pid: 0,
                    pgid: 0,
                    boot_id: "fixture".into(),
                    process_start_time: 0,
                    started_at: 0,
                },
                watcher: SupervisorWatcher {
                    kind: WatcherKind::TakenOverPid,
                    handle: tokio::spawn(async {}),
                },
            })
        })
        .await
        .map(|_| ())
    }
}
