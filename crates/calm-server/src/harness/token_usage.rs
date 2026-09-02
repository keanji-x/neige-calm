//! Context-window occupancy for a spec harness thread (#1255 S3).
//!
//! codex pushes `thread/tokenUsage/updated` after every upstream response.
//! The frame is:
//!
//! ```text
//! params = { threadId, turnId, tokenUsage: {
//!   total:  TokenUsageBreakdown,
//!   last:   TokenUsageBreakdown,
//!   modelContextWindow: number | null,
//! }}
//! TokenUsageBreakdown = { totalTokens, inputTokens, cachedInputTokens,
//!                         cacheWriteInputTokens, outputTokens,
//!                         reasoningOutputTokens }
//! ```
//!
//! That layout is not transcribed from an upstream checkout — the deployed
//! binary self-describes, in about ten seconds and without running a turn:
//!
//! ```text
//! ~/.codex/packages/standalone/current/bin/codex app-server \
//!     generate-json-schema --out /tmp/codex-schema
//! cat /tmp/codex-schema/v2/ThreadTokenUsageUpdatedNotification.json
//! ```
//!
//! Run that whenever this module is suspected of protocol drift. As of
//! 0.151.0 it reports `threadId`/`turnId`/`tokenUsage` all required,
//! `last`/`total` required with `modelContextWindow: integer|null` optional,
//! and `cacheWriteInputTokens` carrying `default: 0` while the other five
//! breakdown fields are required. There is no test for this: the check needs
//! the binary, and a test keyed to an absolute path under someone's home
//! directory would fail everywhere else. `tests/fixtures/
//! thread_token_usage_updated.json` is a real capture out of
//! `~/.codex/sessions`, cross-checked against that generated schema.
//!
//! # `total` is NOT context occupancy. Read this before touching the math.
//!
//! This is the one mistake this module exists to prevent, and it is the
//! mistake the shape of the payload invites: `total` reads like "how full is
//! the context", and it is not. It is a **lifetime sum across every response
//! in the thread** — every turn, every retry, every compaction — so it grows
//! without bound and routinely ends up *orders of magnitude* over the window.
//! Not a guess: the captured frame in
//! `tests/fixtures/thread_token_usage_updated.json` is a real session at
//! `total = 65_570_537` against a `258_400` window — 253.8x. Dividing it by
//! `modelContextWindow` renders a meter at 26607%.
//!
//! The occupancy proxy is **`last.totalTokens`**: the token count of the most
//! recent single upstream response, i.e. what was actually in the model's
//! context the last time it was called. That is the number upstream's own TUI
//! puts in its context bar. [`TokenUsage::used_tokens`] holds it, and it is
//! the only field the percentage is allowed to be computed from.
//!
//! [`TokenUsage::total_tokens`] is kept because it is the honest answer to a
//! *different* question (lifetime cost), which a later slice may want. It is
//! deliberately **not** shipped on `GET /spec/run`: the surest way to
//! reintroduce the bug is to hand the frontend both numbers and let it pick.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Upstream's `BASELINE_TOKENS`, transcribed from `rust-v0.151.0`.
///
/// The first prompt of any thread already carries the system prompt, the tool
/// schemas and the environment preamble — on the order of twelve thousand
/// tokens before the user has typed anything. Measuring occupancy from zero
/// therefore shows a bar that is already substantially full on turn one, which
/// reads as "nearly out of room" when in fact nothing has happened yet.
/// Upstream subtracts this floor from both the numerator and the denominator
/// so the bar starts empty and reaches 100% when the window is actually full.
/// We replicate it on **both** sides for exactly that reason; subtracting it
/// from only the numerator would be a different (and wrong) curve.
pub const BASELINE_TOKENS: i64 = 12_000;

/// Latest context-window usage observed on a harness thread.
///
/// Latest-wins by construction: each frame supersedes the previous one, which
/// is why this rides the runtime snapshot (`worker_sessions.handle_state`,
/// one row per runtime, rewritten in place) rather than being appended to
/// `harness_items`. An append-per-response would need either its own event —
/// a wave-vcs commit plus a 300-row transcript refetch for every model
/// response — or no event at all, in which case no reader would ever see it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// `tokenUsage.last.totalTokens` — the occupancy proxy. See the module
    /// docs: this, never `total_tokens`, is what a percentage may divide.
    pub used_tokens: i64,
    /// `tokenUsage.total.totalTokens` — the thread's lifetime sum. Stored for
    /// a future cost readout; unbounded, and meaningless as a fill ratio.
    pub total_tokens: i64,
    /// `tokenUsage.modelContextWindow`. `None` means "codex has not told us
    /// the window", not "the window is zero" — and a `None` arriving after a
    /// known window does not erase it; see [`TokenUsage::sticky_merge`].
    pub context_window: Option<i64>,
    /// Wall clock of the frame that produced this value. Not used by any
    /// computation here; it exists so a reader can tell a live number from one
    /// rehydrated out of a months-old snapshot — and it is shipped on
    /// `GET /spec/run` (`SpecRunTokenUsage::at_ms`) so that reader can be the
    /// UI, which is the only reader that can act on it. A field whose stated
    /// purpose is "let a reader distinguish" and which no reader receives
    /// would be a doc comment describing a property its carrier does not have.
    pub at_ms: i64,
}

impl TokenUsage {
    /// Parse a `thread/tokenUsage/updated` frame's `params`.
    ///
    /// Returns `None` when `tokenUsage.last.totalTokens` is absent, is not an
    /// integer, **or is negative**. That field is the whole point of the
    /// frame, and a frame without a usable one cannot produce an occupancy
    /// reading — storing a zero would state that the context is empty, which
    /// is a stronger claim than "unknown" and would render as an empty bar
    /// rather than no bar.
    ///
    /// # Why a negative count is rejected here rather than clamped
    ///
    /// A negative token count is not a small number, it is a nonsensical one:
    /// no arithmetic downstream can recover a meaning from it. Clamping it to
    /// zero would make `percent` answer `0.0` — "the context is empty" —
    /// which is exactly the stronger-than-unknown claim this same module
    /// refuses to make two paragraphs above when `last.totalTokens` is
    /// missing. Rejecting the frame keeps the two consistent, and leaves the
    /// *previous* reading in place (the caller merges onto what it already
    /// had), which is a better answer than either a lie or a hole.
    ///
    /// `total.totalTokens` is defaulted to 0 rather than being required, and a
    /// negative one is treated as the same malformed case: it feeds no
    /// computation, and losing an entire usable occupancy reading because the
    /// lifetime counter was garbage would be the wrong trade.
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
        // `modelContextWindow` is `Option<i64>` upstream and serializes as an
        // explicit `null`, so "key present holding null" and "key absent" both
        // land here as `None` — and both mean the same thing to us.
        let context_window = usage.get("modelContextWindow").and_then(Value::as_i64);
        Some(Self {
            used_tokens,
            total_tokens,
            context_window,
            at_ms,
        })
    }

    /// Fold this frame onto whatever was stored before it, keeping a known
    /// window across frames that omit one.
    ///
    /// Upstream models the window as `Option<i64>` on every frame, and a
    /// later `None` there does not mean the window went away — it means this
    /// particular response did not carry it. Overwriting a real window with
    /// `None` would make the meter blink out mid-turn and come back on the
    /// next frame that happened to include it. Counts are always taken from
    /// the incoming frame; only the window is sticky.
    ///
    /// **This is defensive, not load-bearing.** In 181_344 real usage frames
    /// swept out of `~/.codex/sessions` on this box, `model_context_window`
    /// was null or absent in **zero** of them. The `Option` is upstream's
    /// type, so the case is representable and a future version may start
    /// exercising it; nothing observed so far does. Do not read the existence
    /// of this merge as evidence that null windows happen.
    #[must_use]
    pub fn sticky_merge(mut self, previous: Option<&Self>) -> Self {
        if self.context_window.is_none() {
            self.context_window = previous.and_then(|p| p.context_window);
        }
        self
    }

    /// True when the occupancy proxy has overshot the window it is measured
    /// against — the condition under which [`Self::percent`] refuses to
    /// answer. Separated out so the ingest path can log it once per frame
    /// instead of once per HTTP read.
    #[must_use]
    pub fn exceeds_window(&self) -> bool {
        self.context_window
            .is_some_and(|window| self.used_tokens > window)
    }

    /// Context occupancy as a whole percentage in `0.0..=100.0`, or `None`
    /// when no honest percentage exists.
    ///
    /// Computed here, on the server, and shipped as a number — not as two
    /// numbers for the client to divide. There is exactly one baseline
    /// adjustment and exactly one over-window rule, and neither survives
    /// being restated in TypeScript.
    ///
    /// `None` in three cases:
    ///
    /// 1. **No window.** Nothing to be a percentage of.
    /// 2. **A window at or below the baseline.** The denominator would be
    ///    zero or negative. Upstream's baseline assumes a window far larger
    ///    than it; a model that violates that assumption is not something we
    ///    can render, so we decline rather than emit a divide-by-zero.
    /// 3. **`used_tokens > context_window`** — the important one, below.
    ///
    /// # Why case 3 is not a clamp to 100%
    ///
    /// `used_tokens` is a *proxy*: `last.totalTokens` is the size of the most
    /// recent response, which we believe tracks context occupancy. The rule
    /// stands on a measurement, not on a fear — 181_468 real `token_count`
    /// frames (181_344 of them carrying a usage payload) were swept out of
    /// `~/.codex/sessions` on this box:
    ///
    /// - `last > modelContextWindow` in **4 frames (0.002%)**, all four in a
    ///   single anomalous session, where `last` reached 2_361_529 against a
    ///   258_400 window. So the hazard is real and it is rare, which is
    ///   exactly the profile a fail-closed rule is for: it costs a missing
    ///   meter four times in two hundred thousand, and it costs nothing else.
    /// - **The compaction story specifically is not borne out.** The earlier
    ///   version of this comment blamed upstream's `fill_to_context_window`
    ///   writing a delta into `last`. In the sweep, 316 large drops
    ///   (>50_000 tokens, 374 at >10_000) show `last` *falling* across what
    ///   look like compactions — the correct direction, not a delta. That
    ///   sentence was speculation and has been removed; what remains is the
    ///   4-frame anomaly, whose cause is still unknown.
    ///
    /// Whatever the cause, `used > window` is a number that cannot be an
    /// occupancy. Clamping it to a full bar would render the failure as a
    /// normal, plausible "context is full" state and destroy the only
    /// evidence. Withholding the percentage while still shipping the raw
    /// count is honest: the reader sees a token count and no meter, which is
    /// exactly what we know.
    ///
    /// The lower clamp at zero is real (a first response below the baseline
    /// would otherwise go negative). An upper clamp would be dead code: case
    /// 3 has already rejected everything that could exceed 1.
    ///
    /// The subtractions are `saturating_sub` as defence in depth. Ingest
    /// rejects negative counts ([`Self::from_params`]), so nothing should
    /// reach here that could underflow — but "should" is not a guarantee for
    /// a value that also arrives by deserializing a snapshot off disk, and
    /// `i64::MIN - 12_000` panics under the dev/test profile and wraps in
    /// release to a percentage around 3.5e15, which would break both the
    /// documented `0.0..=100.0` range and the never-over-100 rule this
    /// function exists to enforce.
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

    /// A frame as codex sends it, with the two numbers under test spelled
    /// out. `total` here is the lifetime sum, `last` the latest response.
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

    /// THE regression test for this module (#1255 S3), and it runs on real
    /// numbers: `total = 65_570_537` against a `258_400` window — 253.8x over
    /// — captured verbatim from a genuine session (see
    /// `tests/fixtures/thread_token_usage_updated.json`). The percentage must
    /// come from `last` (113_356 → (113356-12000)/(258400-12000) ≈ 41.1%),
    /// and must not be anywhere near the ~26607% that `total` would produce.
    ///
    /// If this goes red after an edit to `percent`, the edit swapped the
    /// occupancy proxy for the lifetime sum. Read the module docs again.
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

    /// An absent `modelContextWindow` key behaves exactly like an explicit
    /// `null` — upstream serializes the `None` case as `null`, but a frame
    /// from a future version that skips the key must not be read differently.
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

    /// Over the window: raw count, no percentage. See `percent`'s docs for
    /// why this is not a clamp.
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

    /// Exactly at the window is not "over" it: 100% is a legal reading, the
    /// boundary belongs to the renderable side.
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

    /// A frame with no `last.totalTokens` is dropped rather than stored as a
    /// zero reading.
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

    /// A negative `last.totalTokens` is rejected at ingest, not clamped.
    ///
    /// Two cases, and they fail differently if the guard is dropped.
    /// `i64::MIN` is the loud one: without the guard, `percent` computes
    /// `i64::MIN - 12_000`, which panics on overflow under the dev/test
    /// profile and wraps in release into a percentage around 3.5e15 —
    /// violating both the documented `0.0..=100.0` range and the
    /// never-over-100 rule. A small negative is the *silent* one: it would
    /// clamp to `0.0%` and render as "context empty", a claim strictly
    /// stronger than "unknown" and one this module refuses to make elsewhere.
    #[test]
    fn a_negative_used_count_is_rejected_rather_than_clamped() {
        for last in [i64::MIN, -1, -4_242] {
            assert!(
                TokenUsage::from_params(&frame(100, last, json!(258_400)), 1).is_none(),
                "a negative last.totalTokens ({last}) is nonsense, not a reading"
            );
        }
    }

    /// A negative lifetime `total` is malformed in a field that feeds no
    /// computation, so it degrades to 0 exactly like an absent one — losing
    /// the whole (usable) occupancy reading over it would be the wrong trade.
    #[test]
    fn a_negative_lifetime_total_degrades_to_zero_and_keeps_the_reading() {
        let usage = TokenUsage::from_params(&frame(-9, 60_000, json!(258_400)), 1)
            .expect("a usable last.totalTokens still yields a reading");
        assert_eq!(usage.total_tokens, 0);
        assert_eq!(usage.used_tokens, 60_000);
    }

    /// Defence in depth for the arithmetic itself: a value that ingest would
    /// have rejected can still arrive by deserializing an old snapshot off
    /// disk, and `percent` must not panic or wrap on it.
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
