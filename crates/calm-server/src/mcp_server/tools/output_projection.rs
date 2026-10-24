//! Gate step display selection; authorization and loading belong to the track reader.
pub(super) fn step_text(text: &str, selected: &str) -> String {
    let mut current = None;
    text.split_inclusive('\n')
        .filter(|line| {
            if let Some(step) = line
                .trim_end_matches(['\r', '\n'])
                .strip_prefix("::gate-step ")
            {
                current = Some(step);
            }
            current == Some(selected)
        })
        .collect()
}
