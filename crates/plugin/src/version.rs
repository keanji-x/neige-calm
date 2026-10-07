//! Semantic-version compatibility check, supplied with the host version.
use semver::Version;
use thiserror::Error;

/// Returned when a plugin requests a newer kernel than we are.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("plugin requires kernel >= {required}, this kernel is {actual}")]
pub struct KernelTooOld {
    pub required: Version,
    pub actual: Version,
}

/// Allow load iff `kernel >= required`; `min_kernel_version` is an inclusive lower bound.
pub fn check_min_kernel_version(kernel: &Version, required: &Version) -> Result<(), KernelTooOld> {
    if kernel >= required {
        Ok(())
    } else {
        Err(KernelTooOld {
            required: required.clone(),
            actual: kernel.clone(),
        })
    }
}
