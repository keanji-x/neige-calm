mod support;

#[path = "cases/boot_invariants.rs"]
mod boot_invariants;
#[path = "cases/inv_02_killpg.rs"]
mod inv_02_killpg;
#[path = "cases/inv_05_pid_ownership_strong.rs"]
mod inv_05_pid_ownership_strong;
#[path = "cases/kernel_reboot_harness.rs"]
mod kernel_reboot_harness;
#[path = "cases/parked_operations.rs"]
mod parked_operations;
#[path = "cases/reconcile_supervisor_on_boot.rs"]
mod reconcile_supervisor_on_boot;
