// Preserve each former target's helper-local test identities in the inventory.
#![allow(clippy::duplicate_mod)]

// Each module retains its former test-target namespace and helper tests.
use planner_card_suite::{common, support};
#[path = "acp_planner_provider.rs"]
mod acp_planner_provider;
#[path = "codex_runtime_suite.rs"]
mod codex_runtime_suite;
#[path = "dispatcher.rs"]
mod dispatcher;
#[path = "kernel_process_suite.rs"]
mod kernel_process_suite;
#[path = "migration_suite.rs"]
mod migration_suite;
#[path = "no_double_spawn.rs"]
mod no_double_spawn;
#[path = "planner_card_suite.rs"]
mod planner_card_suite;
#[path = "runtime_dispatch_suite.rs"]
mod runtime_dispatch_suite;
#[path = "scheduler.rs"]
mod scheduler;
#[path = "suite_registry.rs"]
mod suite_registry;
#[path = "worker_flow_claude_suite.rs"]
mod worker_flow_claude_suite;
#[path = "worker_flow_codex_suite.rs"]
mod worker_flow_codex_suite;
#[path = "worker_flow_driver_suite.rs"]
mod worker_flow_driver_suite;
