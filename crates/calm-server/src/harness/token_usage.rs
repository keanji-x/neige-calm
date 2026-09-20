//! Context-window occupancy for a planner harness thread, from `thread/tokenUsage/updated`.
//! `total` is a LIFETIME sum across every response and routinely exceeds the window;
//! `last.totalTokens` is the occupancy proxy and the only field a percentage may divide.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Upstream's `BASELINE_TOKENS`: the first prompt already carries ~12k tokens of system prompt
/// and tool schemas, so the floor is subtracted from BOTH numerator and denominator.
pub const BASELINE_TOKENS: i64 = 12_000;

/// Latest context-window usage observed on a harness thread; latest-wins, so it rides the
/// runtime snapshot rather than `harness_items`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// `tokenUsage.last.totalTokens` — the occupancy proxy; this, never `total_tokens`, is what a
    /// percentage may divide.
    pub used_tokens: i64,
    /// `tokenUsage.total.totalTokens` — the thread's lifetime sum; unbounded, meaningless as a fill ratio.
    pub total_tokens: i64,
    /// `tokenUsage.modelContextWindow`. `None` means "codex has not told us", and a `None` after a
    /// known window does not erase it; see [`TokenUsage::sticky_merge`].
    pub context_window: Option<i64>,
    /// Wall clock of the frame that produced this value, shipped on `GET /planner/run` so the UI
    /// can tell a live number from one rehydrated out of an old snapshot.
    pub at_ms: i64,
}

impl TokenUsage {
    /// Parse a `thread/tokenUsage/updated` frame's `params`. `None` when `last.totalTokens` is
    /// absent, non-integer, or negative: a zero would claim the context is empty, which is a
    /// stronger claim than "unknown".
    pub fn from_params(params: &Value, at_ms: i64) -> Option<Self> {
        let usage = params.get("tokenUsage")?;
        let used_tokens = usage.get("last")?.get("totalTokens")?.as_i64()?;
        if used_tokens < 0 {
            return None;
        }
        let total_tokens = usage
            .get("total")
            .and_then(|t| t.get("totalTokens"))
            .and_then(Value::as_i64)
            .filter(|total| *total >= 0)
            .unwrap_or(0);
        // `modelContextWindow` serializes as an explicit `null`, so present-null and absent both land here as `None`.
        let context_window = usage.get("modelContextWindow").and_then(Value::as_i64);
        Some(Self {
            used_tokens,
            total_tokens,
            context_window,
            at_ms,
        })
    }

    /// Fold this frame onto the previous one, keeping a known window across frames that omit one
    /// (a later `None` means this response did not carry it). Counts are always the incoming frame's.
    #[must_use]
    pub fn sticky_merge(mut self, previous: Option<&Self>) -> Self {
        if self.context_window.is_none() {
            self.context_window = previous.and_then(|p| p.context_window);
        }
        self
    }

    /// True when the occupancy proxy has overshot the window — the condition under which
    /// [`Self::percent`] refuses to answer. Separated so ingest can log it once per frame.
    #[must_use]
    pub fn exceeds_window(&self) -> bool {
        self.context_window
            .is_some_and(|window| self.used_tokens > window)
    }

    /// Context occupancy as a whole percentage in `0.0..=100.0`, or `None` when no honest percentage
    /// exists: no window, a window at or below the baseline, or `used_tokens > context_window`.
    /// The last is NOT clamped to 100%: `used > window` cannot be an occupancy, and clamping would
    /// render the failure as a plausible "context is full". `saturating_sub` guards snapshot values.
    #[must_use]
    pub fn percent(&self) -> Option<f64> {
        let window = self.context_window?;
        if window <= BASELINE_TOKENS || self.exceeds_window() {
            return None;
        }
        let used = self.used_tokens.saturating_sub(BASELINE_TOKENS).max(0);
        let denominator = window.saturating_sub(BASELINE_TOKENS);
        Some(used as f64 / denominator as f64 * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A frame as codex sends it; `total` is the lifetime sum, `last` the latest response.
    fn frame(total: i64, last: i64, window: Value) -> Value {
        json!({
            "threadId": "t-usage",
            "turnId": "turn-1",
            "tokenUsage": {
                "total": {
                    "totalTokens": total,
                    "inputTokens": total,
                    "cachedInputTokens": 0,
                    "cacheWriteInputTokens": 0,
                    "outputTokens": 0,
                    "reasoningOutputTokens": 0
                },
                "last": {
                    "totalTokens": last,
                    "inputTokens": last,
                    "cachedInputTokens": 0,
                    "cacheWriteInputTokens": 0,
                    "outputTokens": 0,
                    "reasoningOutputTokens": 0
                },
                "modelContextWindow": window
            }
        })
    }

    /// Real numbers: `total = 65_570_537` against a `258_400` window. The percentage must come from
    /// `last`, nowhere near the ~26607% that `total` would produce.
    #[test]
    fn lifetime_total_far_over_window_still_yields_a_last_derived_percent() {
        let usage = TokenUsage::from_params(&frame(65_570_537, 113_356, json!(258_400)), 1)
            .expect("frame carries last.totalTokens");
        assert_eq!(usage.used_tokens, 113_356);
        assert_eq!(usage.total_tokens, 65_570_537);

        let percent = usage.percent().expect("a 113k/258k reading is renderable");
        let expected = 101_356.0 / 246_400.0 * 100.0;
        assert!(
            (percent - expected).abs() < 1e-9,
            "percent must be (last - baseline) / (window - baseline); got {percent}"
        );
        assert!(
            percent < 100.0,
            "a `total`-derived percentage would be ~26607%; got {percent}"
        );
    }

    /// The window is sticky across frames that omit it. Counts are not.
    #[test]
    fn null_context_window_keeps_the_previous_window_and_updates_the_counts() {
        let first = TokenUsage::from_params(&frame(90_000, 30_000, json!(272_000)), 1)
            .expect("first frame")
            .sticky_merge(None);
        assert_eq!(first.context_window, Some(272_000));

        let second = TokenUsage::from_params(&frame(150_000, 45_000, Value::Null), 2)
            .expect("second frame")
            .sticky_merge(Some(&first));

        assert_eq!(
            second.context_window,
            Some(272_000),
            "a null modelContextWindow must not erase a window we already know"
        );
        assert_eq!(second.used_tokens, 45_000, "counts come from the new frame");
        assert_eq!(second.total_tokens, 150_000);
        assert_eq!(second.at_ms, 2);

        let expected = 33_000.0 / 260_000.0 * 100.0;
        let percent = second
            .percent()
            .expect("sticky window makes this renderable");
        assert!((percent - expected).abs() < 1e-9, "got {percent}");
    }

    /// An absent `modelContextWindow` key behaves exactly like an explicit `null`.
    #[test]
    fn missing_context_window_key_is_the_same_as_null() {
        let usage = TokenUsage::from_params(
            &json!({ "tokenUsage": { "last": { "totalTokens": 10 } } }),
            1,
        )
        .expect("last.totalTokens is all that is required");
        assert_eq!(usage.context_window, None);
        assert_eq!(usage.total_tokens, 0, "absent total defaults to zero");
        assert_eq!(usage.percent(), None, "no window, no percentage");
    }

    /// Over the window: raw count, no percentage.
    #[test]
    fn used_above_window_yields_no_percent_and_keeps_the_raw_count() {
        let usage = TokenUsage::from_params(&frame(900_000, 272_001, json!(272_000)), 1)
            .expect("frame carries last.totalTokens");
        assert!(usage.exceeds_window());
        assert_eq!(
            usage.percent(),
            None,
            "an impossible occupancy must be reported as no percentage, not as 100%"
        );
        assert_eq!(
            usage.used_tokens, 272_001,
            "the raw count still ships — it is the evidence"
        );
    }

    /// Exactly at the window is not "over" it: 100% is a legal reading.
    #[test]
    fn used_exactly_at_the_window_is_one_hundred_percent() {
        let usage = TokenUsage::from_params(&frame(900_000, 272_000, json!(272_000)), 1).unwrap();
        assert!(!usage.exceeds_window());
        let percent = usage.percent().expect("at-window is renderable");
        assert!((percent - 100.0).abs() < 1e-9, "got {percent}");
    }

    /// Below the baseline the bar is empty, not negative.
    #[test]
    fn a_first_response_under_the_baseline_floors_at_zero() {
        let usage = TokenUsage::from_params(&frame(8_000, 8_000, json!(272_000)), 1).unwrap();
        assert_eq!(usage.percent(), Some(0.0));
    }

    /// A window at or below the baseline has no usable denominator.
    #[test]
    fn window_at_or_below_the_baseline_yields_no_percent() {
        let usage = TokenUsage::from_params(&frame(1, 1, json!(BASELINE_TOKENS)), 1).unwrap();
        assert_eq!(usage.percent(), None);
    }

    /// A frame with no `last.totalTokens` is dropped rather than stored as a zero reading.
    #[test]
    fn frame_without_last_total_tokens_is_not_parsed() {
        assert!(TokenUsage::from_params(&json!({ "threadId": "t" }), 1).is_none());
        assert!(TokenUsage::from_params(&json!({ "tokenUsage": {} }), 1).is_none());
        assert!(
            TokenUsage::from_params(
                &json!({ "tokenUsage": { "last": { "totalTokens": "60000" } } }),
                1
            )
            .is_none(),
            "a stringified count is not an integer count"
        );
    }

    /// `i64::MIN` is the loud case (`percent` would overflow); a small negative is the silent one
    /// (it would clamp to `0.0%` and render as "context empty").
    #[test]
    fn a_negative_used_count_is_rejected_rather_than_clamped() {
        for last in [i64::MIN, -1, -4_242] {
            assert!(
                TokenUsage::from_params(&frame(100, last, json!(258_400)), 1).is_none(),
                "a negative last.totalTokens ({last}) is nonsense, not a reading"
            );
        }
    }

    /// A negative lifetime `total` feeds no computation, so it degrades to 0 like an absent one.
    #[test]
    fn a_negative_lifetime_total_degrades_to_zero_and_keeps_the_reading() {
        let usage = TokenUsage::from_params(&frame(-9, 60_000, json!(258_400)), 1)
            .expect("a usable last.totalTokens still yields a reading");
        assert_eq!(usage.total_tokens, 0);
        assert_eq!(usage.used_tokens, 60_000);
    }

    /// A value ingest would have rejected can still arrive by deserializing an old snapshot.
    #[test]
    fn percent_does_not_overflow_on_a_negative_used_count_from_a_snapshot() {
        for used_tokens in [i64::MIN, -1] {
            let usage = TokenUsage {
                used_tokens,
                total_tokens: 0,
                context_window: Some(258_400),
                at_ms: 1,
            };
            let percent = usage.percent().expect("a known window is renderable");
            assert_eq!(
                percent, 0.0,
                "a negative count floors at zero rather than overflowing; got {percent}"
            );
        }
    }
}
