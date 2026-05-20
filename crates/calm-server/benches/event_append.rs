//! Microbenchmark for `Repo::write_with_event` overhead — design doc §6.4.
//!
//! **Target: <50µs added per call against `sqlite::memory:`.**
//!
//! Two groups are measured side-by-side:
//!
//!   * `write_with_event_cove_create` — full sync engine write path
//!     (txn open → entity `_tx` → event insert → commit → broadcast).
//!   * `baseline_cove_create_no_event_log` — pre-Scope-A path (entity
//!     write only, no event row, no broadcast).
//!
//! The **delta** between the two is the cost the design budgets at
//! <50µs in-memory. Wall-clock numbers themselves vary with the
//! runtime (current_thread vs multi-thread) and the sqlx pool's per-
//! connection setup; what matters for the PR gate is "no regression
//! worse than +20% on the delta" (design §6.4 verbatim).
//!
//! Run with `cargo bench -p calm-server --bench event_append`.

use std::sync::Arc;

use calm_server::db::Repo;
use calm_server::db::sqlite::{SqlxRepo, cove_create_tx};
use calm_server::db::write_with_event_typed;
use calm_server::event::{Event, EventBus};
use calm_server::model::NewCove;
use criterion::{Criterion, criterion_group, criterion_main};

fn event_append_bench(c: &mut Criterion) {
    // Single tokio runtime shared across all sample runs — avoids the
    // per-iteration runtime spin-up cost which would dominate the
    // measurement we care about.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let (repo, bus): (Arc<dyn Repo>, EventBus) = rt.block_on(async {
        let r: Arc<dyn Repo> = Arc::new(
            SqlxRepo::open("sqlite::memory:")
                .await
                .expect("open in-memory repo"),
        );
        (r, EventBus::new())
    });

    // Pre-subscribe so the broadcast send doesn't take the "no subscribers"
    // fast path (which would understate the cost of a real production
    // emit). We don't drain the receiver — the broadcast channel buffers
    // up to BUS_CAPACITY (1024) before lagging. We re-subscribe between
    // groups to keep the buffer fresh; one group of 100 iterations is
    // well under the cap.
    let _sub = bus.subscribe();

    c.bench_function("write_with_event_cove_create", |b| {
        b.to_async(&rt).iter(|| {
            let repo = Arc::clone(&repo);
            let bus = bus.clone();
            async move {
                let p = NewCove {
                    name: "bench".into(),
                    color: "#000".into(),
                    sort: None,
                };
                let (cove, _id) =
                    write_with_event_typed(repo.as_ref(), "user", None, &bus, move |tx| {
                        Box::pin(async move {
                            let cove = cove_create_tx(tx, p).await?;
                            Ok((cove.clone(), Event::CoveUpdated(cove)))
                        })
                    })
                    .await
                    .unwrap();
                criterion::black_box(cove);
            }
        });
    });

    // Baseline: same cove_create, but bypassing write_with_event — runs
    // the entity insert in a plain `Repo::cove_create` (its own txn, no
    // event log row). Lets reviewers compare the +event-row overhead
    // directly against the existing pre-Scope-A cost.
    c.bench_function("baseline_cove_create_no_event_log", |b| {
        b.to_async(&rt).iter(|| {
            let repo = Arc::clone(&repo);
            async move {
                let p = NewCove {
                    name: "bench".into(),
                    color: "#000".into(),
                    sort: None,
                };
                let cove = repo.cove_create(p).await.unwrap();
                criterion::black_box(cove);
            }
        });
    });
}

criterion_group!(benches, event_append_bench);
criterion_main!(benches);
