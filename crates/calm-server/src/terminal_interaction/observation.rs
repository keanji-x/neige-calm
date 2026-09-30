//! The wall-clock facts of an observation.
use std::time::{SystemTime, UNIX_EPOCH};

/// A wall-clock fact of an observation as milliseconds since the Unix epoch. A time before
/// the epoch is 0, not a panic and not a negative number.
pub(super) fn epoch_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_ms_is_milliseconds_since_the_epoch_and_never_negative() {
        assert_eq!(epoch_ms(UNIX_EPOCH), 0);
        assert_eq!(
            epoch_ms(UNIX_EPOCH + std::time::Duration::from_millis(1_780_977_421_069)),
            1_780_977_421_069
        );
        assert_eq!(
            epoch_ms(UNIX_EPOCH - std::time::Duration::from_secs(1)),
            0,
            "before the epoch: 0, never a panic"
        );
        let now = epoch_ms(SystemTime::now());
        assert!(now > 1_700_000_000_000, "{now}");
    }
}
