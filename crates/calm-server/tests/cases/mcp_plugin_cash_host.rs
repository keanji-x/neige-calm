//! Actual host callback FIFO survives a plugin-side timeout; no DB-wide lock.
use super::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex as StdMutex};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};

thread_local! {
    static CASH_DISPATCH: RefCell<Option<tracing::dispatcher::DefaultGuard>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct CallbackPause {
    target: StdMutex<Option<String>>,
    armed: AtomicBool,
    entered: tokio::sync::Notify,
    released: StdMutex<bool>,
    release: Condvar,
    timed_out: AtomicBool,
}
impl CallbackPause {
    fn arm(&self, track: &str) {
        *self.target.lock().unwrap() = Some(track.to_string());
        self.armed.store(true, Ordering::SeqCst);
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.release.notify_all();
    }
}
struct ReleaseOnDrop(Arc<CallbackPause>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct CallbackPauseLayer(Arc<CallbackPause>);
#[derive(Default)]
struct CallbackFields {
    plugin: Option<String>,
    method: Option<String>,
}
impl tracing::field::Visit for CallbackFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "plugin_id" => self.plugin = Some(value.to_string()),
            "method" => self.method = Some(value.to_string()),
            _ => {}
        }
    }
    fn record_debug(&mut self, _field: &tracing::field::Field, _value: &dyn std::fmt::Debug) {}
}
impl<S: tracing::Subscriber> Layer<S> for CallbackPauseLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if event.metadata().target() != "plugin_host::callback_dispatch"
            || !self.0.armed.load(Ordering::SeqCst)
        {
            return;
        }
        let mut field = CallbackFields::default();
        event.record(&mut field);
        if field.plugin.as_deref() != Some("dev-neige-market")
            || field.method != *self.0.target.lock().unwrap()
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
            .wait_timeout_while(released, Duration::from_secs(35), |released| !*released)
            .unwrap();
        if !*released {
            self.0.timed_out.store(true, Ordering::SeqCst);
        }
    }
}

#[test]
fn cash_callback_timeout_still_orders_readback_and_later_acknowledgements() {
    use tokio::io::AsyncBufReadExt;
    let pause = Arc::new(CallbackPause::default());
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::registry().with(CallbackPauseLayer(Arc::clone(&pause))),
    );
    let worker_dispatch = dispatch.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .on_thread_start(move || {
            CASH_DISPATCH.with(|slot| {
                *slot.borrow_mut() = Some(tracing::dispatcher::set_default(&worker_dispatch))
            })
        })
        .on_thread_stop(|| {
            CASH_DISPATCH.with(|slot| {
                slot.borrow_mut().take();
            })
        })
        .build()
        .unwrap();
    let _main_dispatch = tracing::dispatcher::set_default(&dispatch);
    runtime.block_on(async {
        let fx = boot_fixture_with_options(FixtureOptions {
            assistant_access: false,
            duplicate_tool_name: false,
            market_sina_endpoint: Some(crate::market_plugin_process::sina_server()),
        })
        .await;
        let _release = ReleaseOnDrop(Arc::clone(&pause));
        let (mut rd, mut wr) = connect(&fx.socket_path).await;
        handshake(&mut rd, &mut wr, &fx.assistant_raw_token).await;
        pause.arm("neige.kv.set");
        send_frame(
            &mut wr,
            tools_call_frame(
                2,
                "plugin.dev-neige-market_market.cash.set",
                &fx.assistant_thread_id,
                json!({"currency":"CNY","amount":100}),
            ),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(5), pause.entered.notified())
            .await
            .expect("real cash KV callback must enter dispatch");
        let key = format!("cash/{}", fx.track_id);
        let before = tokio::time::timeout(
            Duration::from_secs(2),
            fx.repo.plugin_kv_get("dev-neige-market", &key),
        )
        .await
        .expect("database itself must remain readable")
        .unwrap();
        assert!(before.is_none());
        // Wait for the actual plugin timeout reply, not a speculative sleep.
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(20), rd.read_line(&mut line))
            .await
            .expect("cash callback must time out")
            .unwrap();
        let timed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(timed["id"], 2);
        assert_eq!(timed["result"]["isError"], true, "{timed}");
        assert!(
            timed["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("timed out"),
            "{timed}"
        );
        send_frame(
            &mut wr,
            tools_call_frame(
                3,
                "plugin.dev-neige-market_market.cash.list",
                &fx.assistant_thread_id,
                json!({}),
            ),
        )
        .await;
        line.clear();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rd.read_line(&mut line))
                .await
                .is_err(),
            "read-back overtook the outstanding write: {line}"
        );
        assert!(!pause.timed_out.load(Ordering::SeqCst));
        pause.release();
        let listed = recv_frame(&mut rd).await;
        assert_eq!(listed["id"], 3);
        assert_eq!(
            listed["result"]["structuredContent"]["balances"],
            json!([{"currency":"CNY","amount":100.0}])
        );
        send_frame(
            &mut wr,
            tools_call_frame(
                4,
                "plugin.dev-neige-market_market.cash.set",
                &fx.assistant_thread_id,
                json!({"currency":"CNY","amount":200}),
            ),
        )
        .await;
        let set = recv_frame(&mut rd).await;
        assert_ne!(set["result"]["isError"], true, "{set}");
        send_frame(
            &mut wr,
            tools_call_frame(
                5,
                "plugin.dev-neige-market_market.cash.list",
                &fx.assistant_thread_id,
                json!({}),
            ),
        )
        .await;
        let final_read = recv_frame(&mut rd).await;
        assert_eq!(
            final_read["result"]["structuredContent"]["balances"],
            json!([{"currency":"CNY","amount":200.0}])
        );
        assert_eq!(
            fx.repo
                .plugin_kv_get("dev-neige-market", &key)
                .await
                .unwrap(),
            Some(json!({"version":1,"balances":[{"currency":"CNY","amount":200.0}]}))
        );
        fx.plugin_host.stop("dev-neige-market").await.unwrap();
    });
}

#[tokio::test]
async fn cash_capacity_and_host_quota_refusal_preserve_all_saved_portfolios() {
    let fx = boot_fixture_with_options(FixtureOptions {
        assistant_access: false,
        duplicate_tool_name: false,
        market_sina_endpoint: Some(crate::market_plugin_process::sina_server()),
    })
    .await;
    let manifest = fx.plugin_host.registry().get("dev-neige-market").unwrap();
    assert_eq!(
        manifest.to_json()["permissions"]["kv_quota_bytes"],
        1_048_576
    );
    let points = vec![json!({"at":"2026-09-09T12:00:00Z","total":100032.46,"currency":"CNY"}); 500];
    let area_id = fx
        .repo
        .track_get(&fx.track_id)
        .await
        .unwrap()
        .unwrap()
        .area_id;
    let mut capacity_tracks = Vec::new();
    for index in 0..12 {
        let track = fx
            .repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area_id.clone(),
                title: format!("Capacity {index}"),
                sort: None,
                cwd: String::new(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: calm_server::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap()
            .id
            .to_string();
        capacity_tracks.push(track.clone());
        for prefix in ["history/", "total_history/"] {
            fx.repo
                .plugin_kv_set(
                    "dev-neige-market",
                    &format!("{prefix}{track}"),
                    &json!(points),
                )
                .await
                .unwrap();
        }
        let holdings = (0..20)
            .map(|n| json!({"asset":format!("US:S{n}"),"quantity":1.0}))
            .collect::<Vec<_>>();
        fx.repo
            .plugin_kv_set(
                "dev-neige-market",
                &format!("holdings/{track}"),
                &json!(holdings),
            )
            .await
            .unwrap();
        fx.repo.plugin_kv_set("dev-neige-market",&format!("cash/{track}"),&json!({"version":1,"balances":[{"currency":"CNY","amount":100.0},{"currency":"USD","amount":100.0},{"currency":"HKD","amount":100.0}]})).await.unwrap();
    }
    let (mut rd, mut wr) = connect(&fx.socket_path).await;
    handshake(&mut rd, &mut wr, &fx.assistant_raw_token).await;
    send_frame(
        &mut wr,
        tools_call_frame(
            2,
            "plugin.dev-neige-market_market.cash.set",
            &fx.assistant_thread_id,
            json!({"currency":"CNY","amount":100}),
        ),
    )
    .await;
    let saved = recv_frame(&mut rd).await;
    assert_ne!(saved["result"]["isError"], true, "{saved}");
    // Await the caller's persisted observation. Capacity fixture symbols are
    // unquoted, so their existing histories cannot grow in this pass.
    let own_history = format!("total_history/{}", fx.track_id);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fx
                .repo
                .plugin_kv_get("dev-neige-market", &own_history)
                .await
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("caller observation must finish before quota measurement");
    let entries = fx
        .repo
        .plugin_kv_list("dev-neige-market", "")
        .await
        .unwrap();
    let used: usize = entries
        .iter()
        .map(|(key, value)| key.len() + serde_json::to_string(value).unwrap().len())
        .sum();
    assert!(used < 1_048_576, "twelve full portfolios use {used} bytes");
    eprintln!("cash capacity fixture: twelve full portfolios plus caller use {used}/1048576 bytes");
    let before = entries
        .iter()
        .filter(|(key, _)| {
            key.rsplit_once('/')
                .is_some_and(|(_, id)| capacity_tracks.iter().any(|track| track == id))
        })
        .cloned()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        before.len(),
        48,
        "all four KV documents on twelve real Tracks are covered"
    );
    // A deliberately full namespace is setup data; the ensuing cash.set goes
    // through the actual host quota check, not this direct Repo helper.
    fx.repo
        .plugin_kv_set(
            "dev-neige-market",
            "quota-fixture",
            &json!("x".repeat(1_048_576 - used - "quota-fixture".len() - 2)),
        )
        .await
        .unwrap();
    send_frame(
        &mut wr,
        tools_call_frame(
            3,
            "plugin.dev-neige-market_market.cash.set",
            &fx.assistant_thread_id,
            json!({"currency":"USD","amount":2}),
        ),
    )
    .await;
    let refused = recv_frame(&mut rd).await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(
        refused["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("quota"),
        "{refused}"
    );
    assert_eq!(
        fx.repo
            .plugin_kv_get("dev-neige-market", &format!("cash/{}", fx.track_id))
            .await
            .unwrap(),
        Some(json!({"version":1,"balances":[{"currency":"CNY","amount":100.0}]}))
    );
    let after = fx
        .repo
        .plugin_kv_list("dev-neige-market", "")
        .await
        .unwrap()
        .into_iter()
        .filter(|(key, _)| {
            key.rsplit_once('/')
                .is_some_and(|(_, id)| capacity_tracks.iter().any(|track| track == id))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(before, after);
    fx.plugin_host.stop("dev-neige-market").await.unwrap();
}
