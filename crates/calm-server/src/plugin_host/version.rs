//! Kernel-version constant + min-kernel-version gate.

use std::sync::LazyLock;

use semver::Version;

/// Kernel version, parsed once from `CARGO_PKG_VERSION`; Cargo guarantees it is semver-shaped.
pub static KERNEL_VERSION: LazyLock<Version> = LazyLock::new(|| {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is valid semver")
});

pub use plugin_runtime::version::{KernelTooOld, check_min_kernel_version};

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn kernel_version_parses() {
        // Forces the LazyLock to evaluate.
        let _: &Version = &KERNEL_VERSION;
    }

    #[test]
    fn lower_required_is_ok() {
        assert!(check_min_kernel_version(&v("0.1.0"), &v("0.0.1")).is_ok());
    }

    #[test]
    fn equal_is_ok() {
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
