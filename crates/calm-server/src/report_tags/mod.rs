//! Structured tags on a track report (#1838 S1): `report_tags` rows keyed by track, ordered by
//! insertion. Never part of the report payload or CRDT body and never parsed from its text.
//! Validation lives here, not in the CLI table, which passes values through unchecked.

pub mod store;

#[cfg(test)]
mod tests;

/// Most tags one report carries after a change.
pub const MAX_TAGS_PER_REPORT: usize = 32;
/// Longest tag, in characters.
pub const MAX_TAG_CHARS: usize = 64;

/// The trimmed tag, or why it is refused. A tag holds no whitespace, control character or `,`:
/// listings print a report's tags space- or comma-joined, so any of those would split one tag in two.
pub fn normalize_tag(raw: &str) -> Result<String, String> {
    let tag = raw.trim();
    if tag.is_empty() {
        return Err("a tag must not be empty or whitespace-only".into());
    }
    if let Some(bad) = tag
        .chars()
        .find(|c| c.is_whitespace() || c.is_control() || *c == ',')
    {
        return Err(format!(
            "tag {tag:?} contains {bad:?}; a tag holds no whitespace, control character or `,`"
        ));
    }
    let chars = tag.chars().count();
    if chars > MAX_TAG_CHARS {
        return Err(format!(
            "tag {tag:?} is {chars} characters; the limit is {MAX_TAG_CHARS}"
        ));
    }
    Ok(tag.to_string())
}
