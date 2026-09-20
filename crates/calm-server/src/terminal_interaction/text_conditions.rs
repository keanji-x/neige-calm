//! Text conditions on a wait: patterns that must be PRESENT (any of `wait_text` on any live
//! viewport row) and patterns that must be ABSENT (none of `wait_text_absent` on any row).
//! Pure functions over rendered rows.
use serde_json::{Value, json};

/// The conditions of one wait. An empty list is vacuously true and reported as `null`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextConditions {
    pub present: Vec<String>,
    pub absent: Vec<String>,
}
/// Which conditions held on the last tested screen; `None` for a condition
/// without patterns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConditionState {
    pub present: Option<bool>,
    pub absent: Option<bool>,
}
impl ConditionState {
    /// Every condition with patterns held.
    pub fn holds(self) -> bool {
        self.present != Some(false) && self.absent != Some(false)
    }
    /// The `wait.conditions` block.
    pub fn to_json(self) -> Value {
        json!({"present":self.present,"absent":self.absent})
    }
}

/// First pattern in argument order, then first row top-down (rows are already trailing-trimmed).
pub fn find_match(patterns: &[String], rows: &[String]) -> Option<(String, usize)> {
    patterns.iter().find_map(|pattern| {
        rows.iter()
            .position(|row| row.contains(pattern.as_str()))
            .map(|row| (pattern.clone(), row))
    })
}

impl TextConditions {
    pub fn is_empty(&self) -> bool {
        self.present.is_empty() && self.absent.is_empty()
    }
    /// Test `rows`: the present match, if any, and the state of both conditions.
    pub fn test(&self, rows: &[String]) -> (Option<(String, usize)>, ConditionState) {
        let matched = find_match(&self.present, rows);
        let state = ConditionState {
            present: (!self.present.is_empty()).then_some(matched.is_some()),
            absent: (!self.absent.is_empty()).then(|| find_match(&self.absent, rows).is_none()),
        };
        (matched, state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn find_match_prefers_the_first_pattern_then_the_first_row() {
        let rows = list(&["a x", "b y", "b z", "", "a"]);
        assert_eq!(find_match(&list(&["b", "a"]), &rows), Some(("b".into(), 1)));
        assert_eq!(find_match(&list(&["a", "b"]), &rows), Some(("a".into(), 0)));
        assert_eq!(
            find_match(&list(&["z", "b z"]), &rows),
            Some(("z".into(), 2))
        );
        assert_eq!(find_match(&list(&["q"]), &rows), None);
        assert_eq!(find_match(&list(&["a"]), &[]), None);
    }

    #[test]
    fn conditions_report_each_side_and_hold_on_their_conjunction() {
        let rows = list(&["❯ answer", "· Noodling… (esc to interrupt)"]);
        let present = TextConditions {
            present: list(&["❯"]),
            absent: vec![],
        };
        let (matched, state) = present.test(&rows);
        assert_eq!(matched, Some(("❯".into(), 0)));
        assert_eq!(
            state,
            ConditionState {
                present: Some(true),
                absent: None
            }
        );
        assert!(state.holds());
        assert_eq!(state.to_json(), json!({"present":true,"absent":null}));
        let absent = TextConditions {
            present: vec![],
            absent: list(&["esc to interrupt"]),
        };
        let (matched, state) = absent.test(&rows);
        assert_eq!(matched, None, "no present pattern, no match");
        assert_eq!(
            state,
            ConditionState {
                present: None,
                absent: Some(false)
            }
        );
        assert!(!state.holds());
        let (matched, state) = absent.test(&list(&["❯ answer"]));
        assert_eq!(matched, None);
        assert_eq!(state.absent, Some(true));
        assert!(state.holds());
        let both = TextConditions {
            present: list(&["❯"]),
            absent: list(&["esc to interrupt"]),
        };
        let (matched, state) = both.test(&rows);
        assert_eq!(matched, Some(("❯".into(), 0)), "the match is named");
        assert_eq!(
            state,
            ConditionState {
                present: Some(true),
                absent: Some(false)
            }
        );
        assert!(!state.holds(), "present held, absent did not");
        let (_, state) = both.test(&list(&["done"]));
        assert_eq!(
            state,
            ConditionState {
                present: Some(false),
                absent: Some(true)
            }
        );
        assert!(!state.holds());
        let (_, state) = both.test(&list(&["❯ done"]));
        assert!(state.holds());
        assert_eq!(state.to_json(), json!({"present":true,"absent":true}));
        assert!(TextConditions::default().is_empty());
        assert!(!both.is_empty() && !absent.is_empty());
        let (matched, state) = TextConditions::default().test(&rows);
        assert_eq!((matched, state), (None, ConditionState::default()));
        assert!(state.holds(), "no conditions hold vacuously");
        assert_eq!(state.to_json(), json!({"present":null,"absent":null}));
    }
}
