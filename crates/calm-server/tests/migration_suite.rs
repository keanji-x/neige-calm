mod support;

#[path = "cases/assistant_provider_migration.rs"]
mod assistant_provider_migration;
#[path = "cases/handle_state_writers.rs"]
mod handle_state_writers;
#[path = "cases/mails_cascade.rs"]
mod mails_cascade;
#[path = "cases/migration_0014_backfill.rs"]
mod migration_0014_backfill;
#[path = "cases/migration_0095_queue_harvested_backfill.rs"]
mod migration_0095_queue_harvested_backfill;
#[path = "cases/migration_replay_harness.rs"]
mod migration_replay_harness;
#[path = "cases/worker_sessions_row_disappearance.rs"]
mod worker_sessions_row_disappearance;
