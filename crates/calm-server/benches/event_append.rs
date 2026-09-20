//! Microbenchmark for `Repo::write_with_event` overhead: the delta between the event-logged and baseline groups is what matters.
//! Run with `cargo bench -p calm-server --bench event_append`.

use std::sync::Arc;

use calm_server::db::Repo;
use calm_server::db::sqlite::{SqlxRepo, area_create_tx};
use calm_server::db::write_with_event_typed;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::NewArea;
use criterion::{Criterion, criterion_group, criterion_main};

fn event_append_bench(c: &mut Criterion) {
    // One runtime across all sample runs, so runtime spin-up does not dominate the measurement.
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

    // Pre-subscribe so the broadcast send does not take the "no subscribers" fast path; the receiver is never drained and one group stays well under BUS_CAPACITY.
    let _sub = bus.subscribe();

    c.bench_function("write_with_event_area_create", |b| {
        b.to_async(&rt).iter(|| {
            let repo = Arc::clone(&repo);
            let bus = bus.clone();
            async move {
                let p = NewArea {
                    name: "bench".into(),
                    color: "#000".into(),
                    sort: None,
                };
                let (area, _id) = write_with_event_typed(
                    repo.as_ref(),
                    ActorId::User,
                    EventScope::System,
                    None,
                    &bus,
                    &calm_server::state::WriteContext::new(
                        calm_server::card_role_cache::CardRoleCache::new(),
                        calm_server::track_area_cache::TrackAreaCache::new(),
                    ),
                    move |tx| {
                        Box::pin(async move {
                            let area = area_create_tx(tx, p).await?;
                            Ok((area.clone(), Event::AreaUpdated(area)))
                        })
                    },
                )
                .await
                .unwrap();
                criterion::black_box(area);
            }
        });
    });

    // Baseline: same area_create via plain `Repo::area_create` (own txn, no event log row).
    c.bench_function("baseline_area_create_no_event_log", |b| {
        b.to_async(&rt).iter(|| {
            let repo = Arc::clone(&repo);
            async move {
                let p = NewArea {
                    name: "bench".into(),
                    color: "#000".into(),
                    sort: None,
                };
                let area = repo.area_create(p).await.unwrap();
                criterion::black_box(area);
            }
        });
    });
}

criterion_group!(benches, event_append_bench);
criterion_main!(benches);
