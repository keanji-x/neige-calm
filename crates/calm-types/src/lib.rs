//! IO-free vocabulary shared by the calm kernel crates.
//!
//! This bottom layer contains no sqlx, axum, tokio, or other IO dependencies.

pub mod boot_budget;
pub mod compatibility;
pub mod error;
pub mod event;
pub mod forge_git;
pub mod harness;
pub mod ids;
pub mod mcp_connector;
pub mod mobile_access;
pub mod model;
pub mod observation;
pub mod planner_attachment;
pub mod proposal;
pub mod report_blocks;
pub mod report_links;
pub mod runtime;
pub mod task_recovery;
pub mod track_fs_dto;
pub mod track_lifecycle;
pub mod track_report;
pub mod worker;
pub mod worker_flow;

pub mod task_execution;
