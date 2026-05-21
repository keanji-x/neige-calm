//! Kernel-version constant + min-kernel-version gate.
//!
//! The plugin manifest carries a `min_kernel_version` field which the schema
//! validator (in `manifest.rs`) already confirms parses as semver. This module
//! owns the *comparison*: at plugin load time we refuse to start any plugin
//! whose `min_kernel_version` exceeds the kernel's own version.
//!
//! Issue #45 — without this gate, a plugin claiming "needs kernel 99.0.0"
//! happily loads against kernel 0.1.0 and only fails (or, worse, silently
//! mis-behaves) when it discovers a missing capability. Surfacing the
//! incompatibility up-front avoids confusing late-binding failures and gives
//! operators a clear log line / 4xx error pointing at the version mismatch.
//!
//! The constant pulls from `CARGO_PKG_VERSION` of `calm-server` so it tracks
//! the workspace's release stamp without a separate manual bump. The parse is
//! deferred to first access via `LazyLock` — `Version::parse` allocates and we
//! don't want it on every host-construction hot path.
//!
//! `check_min_kernel_version` is intentionally a free function: it carries no
//! state, takes both versions by reference, and returns a typed error. Keeping
//! it standalone lets the unit tests cover every interesting boundary without
//! constructing a full `PluginHost`.

use std::sync::LazyLock;

use semver::Version;
use thiserror::Error;

/// Kernel version, parsed once from `calm-server`'s `Cargo.toml`. The
/// `expect` is safe-by-construction: Cargo guarantees `CARGO_PKG_VERSION` is
/// semver-shaped, and the workspace would fail to build if it weren't.
pub static KERNEL_VERSION: LazyLock<Version> = LazyLock::new(|| {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is valid semver")
});

/// Returned when a plugin requests a newer kernel than we are. Carries both
/// versions so callers (REST handlers, log lines) can render a clear message
/// without re-fetching `KERNEL_VERSION`.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("plugin requires kernel >= {required}, this kernel is {actual}")]
pub struct KernelTooOld {
    pub required: Version,
    pub actual: Version,
}

/// Allow load iff `kernel >= required`. Equality is treated as compatible —
/// `min_kernel_version` is an inclusive lower bound, matching how semver
/// "minimum compatible version" comparators (`>=`) work in Cargo and the
/// broader ecosystem.
pub fn check_min_kernel_version(
    kernel: &Version,
    required: &Version,
) -> Result<(), KernelTooOld> {
    if kernel >= required {
        Ok(())
    } else {
        Err(KernelTooOld {
            required: required.clone(),
            actual: kernel.clone(),
        })
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn kernel_version_parses() {
        // Forces the LazyLock to evaluate. If `CARGO_PKG_VERSION` ever stops
        // being valid semver, this test catches it before any plugin load
        // path panics at runtime.
        let _: &Version = &KERNEL_VERSION;
    }

    #[test]
    fn lower_required_is_ok() {
        assert!(check_min_kernel_version(&v("0.1.0"), &v("0.0.1")).is_ok());
    }

    #[test]
    fn equal_is_ok() {
        // `min_kernel_version` is inclusive — a plugin pinned exactly to the
        // running kernel must load.
        assert!(check_min_kernel_version(&v("0.1.0"), &v("0.1.0")).is_ok());
    }

    #[test]
    fn higher_required_is_err() {
        let err = check_min_kernel_version(&v("0.1.0"), &v("0.2.0")).unwrap_err();
        assert_eq!(err.required, v("0.2.0"));
        assert_eq!(err.actual, v("0.1.0"));
    }

    #[test]
    fn major_bump_higher_required_is_err() {
        let err = check_min_kernel_version(&v("0.9.9"), &v("1.0.0")).unwrap_err();
        assert_eq!(err.required, v("1.0.0"));
        assert_eq!(err.actual, v("0.9.9"));
    }

    #[test]
    fn major_kernel_above_required_is_ok() {
        assert!(check_min_kernel_version(&v("1.0.0"), &v("0.5.0")).is_ok());
    }

    #[test]
    fn display_message_includes_both_versions() {
        let err = check_min_kernel_version(&v("0.1.0"), &v("0.2.0")).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("0.2.0"), "missing required: {s}");
        assert!(s.contains("0.1.0"), "missing actual: {s}");
    }
}
