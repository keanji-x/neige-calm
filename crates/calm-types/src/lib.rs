//! calm-types — shared vocabulary for the calm kernel (issue #679, PR1).
//!
//! Bottom of the crate DAG. Holds the types every other crate speaks:
//!
//! ```text
//! ids             CoveId / WaveId / CardId / ActorId (frozen — see ids.rs)
//! model           entity types + pure DTOs (Cove/Wave/Card/Overlay/…)
//! event           Event wire enum + EventScope + payload data types
//!                 (EventBus / BroadcastEnvelope stay in calm-server —
//!                 they are tokio-broadcast IO, not vocabulary)
//! wave_lifecycle  the (from, to, actor) edge table — pure validator
//! runtime         runtime projection vocabulary (WorkerSessionState / RuntimeKind /
//!                 WorkerSessionProjection…) — the repo trait stays in calm-server
//! observation     spec-harness Observation enum (pure data, persisted in
//!                 harness snapshots and spoken by calm-exec's seams)
//! harness         HarnessPhaseTag (the TS-exported phase discriminator)
//! wave_fs_dto     read-only wave-file JSON projection DTOs
//! wave_report     WaveReportPayload (Tier-A persisted card payload)
//! error           CoreError — the IO-free half of calm-server's CalmError
//! worker          NEW (#679): Principal / WorkerContract / SessionMode /
//!                 Liveness / ExitEvidence / WorkerSession vocabulary
//! ```
//!
//! ## Hard rule: zero IO dependencies
//!
//! No sqlx, no axum, no tokio (issue #679 crate-decomposition table). The
//! entities here are plain data; row mapping lives in calm-server's
//! `db::rows` during the shim window and moves to calm-truth in PR2. HTTP
//! error mapping (`IntoResponse`) stays on calm-server's `CalmError`, which
//! wraps [`error::CoreError`] (the "two-stage enum" split).

pub mod error;
pub mod event;
pub mod harness;
pub mod ids;
pub mod model;
pub mod observation;
pub mod runtime;
pub mod wave_fs_dto;
pub mod wave_lifecycle;
pub mod wave_report;
pub mod worker;
pub mod worker_flow;
