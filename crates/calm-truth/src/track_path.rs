//! Paths in the track virtual filesystem, shared by live views and VCS snapshots.

/// Trim surrounding whitespace and slashes; the virtual root is the empty string.
/// Interior separators and path components are preserved verbatim.
pub fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed == "/" {
        return String::new();
    }
    trimmed
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod normalization_tests {
    #[test]
    fn normalize_track_path_preserves_virtual_path_contract() {
        let cases = [
            ("", ""),
            (" \t\n", ""),
            ("/", ""),
            ("////", ""),
            ("///cards///", "cards"),
            ("  /cards/card-1/meta.json/ ", "cards/card-1/meta.json"),
            ("cards/card-1/meta.json", "cards/card-1/meta.json"),
            ("/cards//card-1/", "cards//card-1"),
            ("/ cards /", " cards "),
            ("/cards/../meta.json/", "cards/../meta.json"),
        ];
        for (input, expected) in cases {
            assert_eq!(super::normalize_path(input), expected, "input: {input:?}");
        }
    }
}
