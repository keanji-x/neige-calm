//! Exercise the real socket dispatcher across a real plugin generation change.
//! The existing scope-resolution trace is an observation barrier, not a new
//! production hook. A bounded blocking subscriber parks one runtime worker;
//! the other worker can finish the reload before dispatch is allowed to resume.
use super::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex as StdMutex};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};

thread_local! {
    static REVIEW_DISPATCH: RefCell<Option<tracing::dispatcher::DefaultGuard>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct ScopePause {
    target: StdMutex<Option<String>>,
    armed: AtomicBool,
    entered: tokio::sync::Notify,
    released: StdMutex<bool>,
    release: Condvar,
    timed_out: AtomicBool,
}
impl ScopePause {
    fn arm(&self, track: &str) {
        *self.target.lock().unwrap() = Some(track.to_string());
        self.armed.store(true, Ordering::SeqCst);
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.release.notify_all();
    }
}
struct ReleaseOnDrop(Arc<ScopePause>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct ScopePauseLayer(Arc<ScopePause>);
#[derive(Default)]
struct TrackField(Option<String>);
impl tracing::field::Visit for TrackField {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "track_id" {
            self.0 = Some(value.to_string());
        }
    }
    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}
impl<S: tracing::Subscriber> Layer<S> for ScopePauseLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "mcp_server::tool_visibility"
            || !self.0.armed.load(Ordering::SeqCst)
        {
            return;
        }
        let mut field = TrackField::default();
        event.record(&mut field);
        if field.0 != *self.0.target.lock().unwrap()
            || self
                .0
                .armed
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return;
        }
        self.0.entered.notify_one();
        let released = self.0.released.lock().unwrap();
        let (released, _) = self
            .0
            .release
            .wait_timeout_while(released, Duration::from_secs(20), |released| !*released)
            .unwrap();
        if !*released {
            self.0.timed_out.store(true, Ordering::SeqCst);
        }
    }
}

#[test]
fn assistant_opt_in_cannot_cross_a_reload_that_revokes_access() {
    let pause = Arc::new(ScopePause::default());
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::registry().with(ScopePauseLayer(Arc::clone(&pause))),
    );
    let worker_dispatch = dispatch.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .on_thread_start(move || {
            REVIEW_DISPATCH.with(|slot| {
                *slot.borrow_mut() = Some(tracing::dispatcher::set_default(&worker_dispatch));
            })
        })
        .on_thread_stop(|| {
            REVIEW_DISPATCH.with(|slot| {
                slot.borrow_mut().take();
            })
        })
        .build()
        .unwrap();
    let _main_dispatch = tracing::dispatcher::set_default(&dispatch);
    runtime.block_on(async {
        let fx = boot_fixture_with_options(FixtureOptions {
            assistant_access: true,
            duplicate_tool_name: false,
            market_sina_endpoint: None,
        })
        .await;
        // Declared after the fixture so failure releases the blocked dispatcher
        // before fixture/runtime teardown. The subscriber also has its own cap.
        let _release_on_drop = ReleaseOnDrop(Arc::clone(&pause));
        let before_client = fx.plugin_host.mcp_client(PLUGIN_ID).await.unwrap();
        let (mut rd, mut wr) = connect(&fx.socket_path).await;
        handshake(&mut rd, &mut wr, &fx.assistant_raw_token).await;
        pause.arm(&fx.track_id);
        send_frame(
            &mut wr,
            tools_call_frame(
                2,
                EXPOSED_NAME,
                &fx.assistant_thread_id,
                json!({"payload":"during-reload"}),
            ),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), pause.entered.notified())
            .await
            .expect("the real dispatcher did not reach scope resolution");

        let mut manifest = fx.plugin_host.registry().get(PLUGIN_ID).unwrap().to_json();
        manifest["exposes_tools"][0]["assistant_access"] = json!(false);
        // Existing fixture boot seeds the registry and DB, so create the actual
        // installed manifest that the public reload path reads.
        std::fs::write(
            fx._tmp
                .path()
                .join("plugins")
                .join(PLUGIN_ID)
                .join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(10), fx.plugin_host.reload(PLUGIN_ID))
            .await
            .expect("reload hung while the scope callback was parked")
            .expect("reload must finish before the held call resumes");
        let after_client = fx.plugin_host.mcp_client(PLUGIN_ID).await.unwrap();
        assert!(
            !Arc::ptr_eq(&before_client, &after_client),
            "reload must replace the real client"
        );
        assert!(
            !fx.plugin_host
                .registry()
                .get(PLUGIN_ID)
                .unwrap()
                .exposes_tools[0]
                .assistant_access
        );
        assert!(
            !pause.timed_out.load(Ordering::SeqCst),
            "barrier expired: the interleaving was not controlled"
        );
        pause.release();
        let result = tokio::time::timeout(Duration::from_secs(5), recv_frame(&mut rd))
            .await
            .expect("held dispatch did not return");

        // The new tool really remains running/reachable for its existing role.
        // This excludes a vacuous refusal caused by failing to restart a child.
        let (mut worker_rd, mut worker_wr) = connect(&fx.socket_path).await;
        handshake(&mut worker_rd, &mut worker_wr, &fx.raw_token).await;
        send_frame(
            &mut worker_wr,
            tools_call_frame(
                3,
                EXPOSED_NAME,
                &fx.thread_id,
                json!({"payload":"worker-control"}),
            ),
        )
        .await;
        let control = recv_frame(&mut worker_rd).await;
        for id in [
            PLUGIN_ID,
            COLLIDING_PLUGIN_ID,
            fx.trusted_plugin_id.as_str(),
        ] {
            fx.plugin_host.stop(id).await.expect("stop fixture plugin");
        }
        assert!(
            control.get("error").is_none() && control["result"]["isError"] != true,
            "replacement tool must still work for Worker: {control:#?}"
        );
        assert!(
            result.get("error").is_some() || result["result"]["isError"] == true,
            "old Assistant permission was applied to the newly revoked client: {result:#?}"
        );
    });
}
