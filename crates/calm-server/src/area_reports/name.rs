//! Track title <-> `area/reports/` file name (#1838 S2). A file name is
//! `escape(title)` + optional `~<suffix>` + `.md`:
//!
//! * `escape` percent-encodes (`%XX`, uppercase hex, one per UTF-8 byte) exactly `%`, `/`, `~`, `\`,
//!   every control character, and a `.` that begins the title, so no name is `.`/`..`/hidden or
//!   holds a path separator. Everything else (CJK, spaces, other punctuation) stays literal.
//! * `~` in a title is always escaped, so a literal `~` only ever starts the ID suffix: an escaped
//!   title can never be mistaken for a generated suffix, and the last `~` splits the two.
//! * Titles shared by 2+ tracks in the area list as `<name>~<suffix>.md` for EVERY such track; the
//!   suffix is the shortest prefix of the track ID, at least [`SUFFIX_MIN_CHARS`] long, that no other
//!   track of that group shares. An empty title always carries its suffix (`~<suffix>.md`): the bare
//!   name `.md` would be hidden, so it is never listed and never resolves.
//! * Parsing accepts only what `escape` produces: a malformed or non-canonical escape (`%2f`, `%41`,
//!   an unescaped `~` in the name, ...) is refused rather than guessed.
//!
//! The name is a view of the title, not an identity: a rename changes it and leaves no alias.

use std::collections::HashMap;

/// The file extension every report name carries.
pub const EXTENSION: &str = ".md";
/// Shortest generated ID suffix.
pub const SUFFIX_MIN_CHARS: usize = 8;

fn must_escape(index: usize, c: char) -> bool {
    matches!(c, '%' | '/' | '~' | '\\') || c.is_control() || (index == 0 && c == '.')
}

/// The title as it appears in a file name; see the module doc.
pub fn escape(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for (index, c) in title.chars().enumerate() {
        if must_escape(index, c) {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{byte:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The title `name` escapes, accepting only [`escape`]'s exact output.
pub fn unescape(name: &str) -> Result<String, String> {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes
                .get(index + 1..index + 3)
                .and_then(|pair| std::str::from_utf8(pair).ok())
                .filter(|pair| pair.chars().all(|c| matches!(c, '0'..='9' | 'A'..='F')))
                .ok_or_else(|| {
                    format!("`{name}` has a malformed escape; use `%XX` with uppercase hex")
                })?;
            out.push(u8::from_str_radix(hex, 16).expect("two uppercase hex digits"));
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    let title =
        String::from_utf8(out).map_err(|_| format!("`{name}` escapes bytes that are not UTF-8"))?;
    if escape(&title) != name {
        return Err(format!(
            "`{name}` is not a listed name (its canonical form is `{}`)",
            escape(&title)
        ));
    }
    Ok(title)
}

/// A report file name taken apart.
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedName {
    pub title: String,
    /// A track-ID prefix, when the name carries `~<suffix>`.
    pub suffix: Option<String>,
}

/// Parse one file name (no directory part) of `area/reports/`.
pub fn parse(file: &str) -> Result<ParsedName, String> {
    let stem = file
        .strip_suffix(EXTENSION)
        .ok_or_else(|| format!("`{file}` is not a report name: report names end in `.md`"))?;
    let (name, suffix) = match stem.rsplit_once('~') {
        Some((name, suffix)) => (name, Some(suffix)),
        None => (stem, None),
    };
    if let Some(suffix) = suffix
        && (suffix.is_empty()
            || !suffix
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    {
        return Err(format!(
            "`{file}` has an invalid `~` ID suffix; a suffix is a prefix of a track ID"
        ));
    }
    let title = unescape(name)?;
    if title.is_empty() && suffix.is_none() {
        return Err(format!(
            "`{file}` is not a report name: an untitled report is listed as `~<id>.md`"
        ));
    }
    Ok(ParsedName {
        title,
        suffix: suffix.map(str::to_string),
    })
}

/// The listed file name of each `(title, track_id)`, in the same order. Suffixes are computed over
/// exactly this set, so pass every report of the area, never a filtered subset.
pub fn file_names(reports: &[(&str, &str)]) -> Vec<String> {
    let mut groups: HashMap<&str, Vec<&str>> = HashMap::new();
    for (title, track_id) in reports {
        groups.entry(title).or_default().push(track_id);
    }
    reports
        .iter()
        .map(|(title, track_id)| {
            let group = &groups[title];
            let mut name = escape(title);
            if group.len() > 1 || title.is_empty() {
                name.push('~');
                name.push_str(unique_prefix(track_id, group));
            }
            name.push_str(EXTENSION);
            name
        })
        .collect()
}

/// The shortest prefix of `id`, at least [`SUFFIX_MIN_CHARS`] long, that no other id in `group` starts with.
fn unique_prefix<'a>(id: &'a str, group: &[&str]) -> &'a str {
    let others: Vec<&str> = group.iter().copied().filter(|other| *other != id).collect();
    let mut ends: Vec<usize> = id
        .char_indices()
        .map(|(index, _)| index)
        .skip(1)
        .chain([id.len()])
        .collect();
    ends.retain(|end| id[..*end].chars().count() >= SUFFIX_MIN_CHARS || *end == id.len());
    ends.into_iter()
        .map(|end| &id[..end])
        .find(|prefix| !others.iter().any(|other| other.starts_with(prefix)))
        .unwrap_or(id)
}
