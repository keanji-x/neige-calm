//! The front matter of a template file: a TOML block between two `+++` lines, then the report body,
//! returned byte for byte. An id matches `^[a-z0-9][a-z0-9-]*$`, so a file can never spell the `site/` prefix.

use serde::Deserialize;

/// The two facts a template file declares about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontMatter {
    pub id: String,
    pub title: String,
}

/// `deny_unknown_fields`: a typo'd key is a broken file, not a file with one fewer fact.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFrontMatter {
    id: String,
    title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontMatterError {
    /// The text does not start with the opening delimiter line `+++\n`.
    MissingOpen,
    /// No line after the opening delimiter is exactly `+++`.
    Unclosed,
    /// The TOML between the delimiters did not deserialize: syntax, a missing
    /// key, or an unknown one.
    Toml(String),
    /// `id` is not `^[a-z0-9][a-z0-9-]*$`.
    BadId(String),
}

impl std::fmt::Display for FrontMatterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingOpen => {
                write!(f, "template file must open with a `+++` front matter line")
            }
            Self::Unclosed => write!(
                f,
                "template front matter has no closing line that is exactly `+++` (no \
                 trailing whitespace or CR)"
            ),
            Self::Toml(message) => write!(f, "template front matter: {message}"),
            Self::BadId(id) => write!(
                f,
                "template front matter: id `{id}` must match ^[a-z0-9][a-z0-9-]*$ \
                 (no `/`: the `site/` and `plugin/` prefixes are reserved for the \
                 loader, never spelled in a file)"
            ),
        }
    }
}

impl std::error::Error for FrontMatterError {}

/// The delimiter line, with its newline: a delimiter is a whole line.
const DELIMITER: &str = "+++\n";

/// Split a template file into its front matter and its body. The body is a borrow of `text`,
/// not a copy, so a `'static` source yields a `'static` body (the builtin roster relies on that).
pub fn parse(text: &str) -> Result<(FrontMatter, &str), FrontMatterError> {
    let after_open = text
        .strip_prefix(DELIMITER)
        .ok_or(FrontMatterError::MissingOpen)?;
    // The closing line: either the very next line (empty front matter, which
    // then fails on the missing keys) or a `+++` line preceded by a newline.
    let (toml_text, body) = if let Some(body) = after_open.strip_prefix(DELIMITER) {
        ("", body)
    } else {
        let close_at = after_open
            .find("\n+++\n")
            .ok_or(FrontMatterError::Unclosed)?;
        (
            &after_open[..close_at + 1],
            &after_open[close_at + 1 + DELIMITER.len()..],
        )
    };
    let raw: RawFrontMatter =
        toml::from_str(toml_text).map_err(|error| FrontMatterError::Toml(error.to_string()))?;
    if !is_valid_id(&raw.id) {
        return Err(FrontMatterError::BadId(raw.id));
    }
    Ok((
        FrontMatter {
            id: raw.id,
            title: raw.title,
        },
        body,
    ))
}

/// `^[a-z0-9][a-z0-9-]*$`, spelled out rather than compiled.
fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const WELL_FORMED: &str = "+++\nid = \"small-change\"\ntitle = \"Small change\"\n+++\n\
<!-- neige:contract {} -->\n\n# Plan\n\n```neige-block task\n{}\n```\n\n";

    #[test]
    fn round_trip_yields_both_fields_and_the_byte_exact_body() {
        let (front, body) = parse(WELL_FORMED).expect("well-formed");
        assert_eq!(
            front,
            FrontMatter {
                id: "small-change".into(),
                title: "Small change".into(),
            }
        );
        // Byte-exact: the returned slice is the input's own bytes (pointer identity), not a copy.
        let expected_body =
            "<!-- neige:contract {} -->\n\n# Plan\n\n```neige-block task\n{}\n```\n\n";
        assert_eq!(body, expected_body);
        let body_offset = WELL_FORMED.len() - expected_body.len();
        assert!(std::ptr::eq(
            body.as_ptr(),
            WELL_FORMED[body_offset..].as_ptr()
        ));
        assert_eq!(
            &WELL_FORMED[body_offset - DELIMITER.len()..body_offset],
            DELIMITER
        );
    }

    #[test]
    fn a_body_with_leading_blank_lines_and_no_trailing_newline_is_kept_verbatim() {
        let text = "+++\nid = \"x\"\ntitle = \"X\"\n+++\n\n\n  # not trimmed";
        let (_, body) = parse(text).expect("well-formed");
        assert_eq!(body, "\n\n  # not trimmed");
    }

    #[test]
    fn an_empty_body_is_allowed_by_the_parser() {
        // The parser does not judge the body; whether an empty one is usable is decided downstream.
        let (front, body) = parse("+++\nid = \"x\"\ntitle = \"X\"\n+++\n").expect("well-formed");
        assert_eq!(front.id, "x");
        assert_eq!(body, "");
    }

    #[test]
    fn missing_front_matter_is_refused() {
        for text in [
            "",
            "# Plan\n",
            "<!-- neige:contract {} -->\n",
            " +++\nid = \"x\"\ntitle = \"X\"\n+++\n",
            "+++id = \"x\"\n+++\n",
            "++\nid = \"x\"\ntitle = \"X\"\n+++\n",
            "+++\r\nid = \"x\"\ntitle = \"X\"\n+++\r\n",
        ] {
            assert_eq!(parse(text), Err(FrontMatterError::MissingOpen), "{text:?}");
        }
    }

    #[test]
    fn unclosed_front_matter_is_refused() {
        for text in [
            "+++\nid = \"x\"\ntitle = \"X\"\n",
            "+++\nid = \"x\"\ntitle = \"X\"\n+++",
            "+++\nid = \"x\"\ntitle = \"X\"\n +++\n",
            "+++\nid = \"x\"\ntitle = \"X\"\n++++\n",
            "+++\nid = \"x\"\ntitle = \"X\"\n+++ \n",
            "+++\nid = \"x\"\ntitle = \"X\"\n+++\r\n",
        ] {
            assert_eq!(parse(text), Err(FrontMatterError::Unclosed), "{text:?}");
        }
    }

    #[test]
    fn the_first_bare_delimiter_line_closes_even_when_the_body_has_one() {
        let text = "+++\nid = \"x\"\ntitle = \"X\"\n+++\nbody\n+++\nmore\n";
        let (_, body) = parse(text).expect("well-formed");
        assert_eq!(body, "body\n+++\nmore\n");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let error = parse("+++\nid = \"x\"\ntitle = \"X\"\nkind = \"template\"\n+++\n")
            .expect_err("unknown key");
        match error {
            FrontMatterError::Toml(message) => {
                assert!(message.contains("kind"), "{message}");
                assert!(message.contains("unknown field"), "{message}");
            }
            other => panic!("expected Toml, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_key_is_refused() {
        for (text, missing) in [
            ("+++\ntitle = \"X\"\n+++\n", "id"),
            ("+++\nid = \"x\"\n+++\n", "title"),
            ("+++\n+++\n", "id"),
        ] {
            match parse(text) {
                Err(FrontMatterError::Toml(message)) => {
                    assert!(message.contains("missing field"), "{text:?}: {message}");
                    assert!(message.contains(missing), "{text:?}: {message}");
                }
                other => panic!("{text:?}: expected Toml(missing field), got {other:?}"),
            }
        }
    }

    #[test]
    fn toml_syntax_errors_are_refused() {
        assert!(matches!(
            parse("+++\nid = small-change\ntitle = \"X\"\n+++\n"),
            Err(FrontMatterError::Toml(_))
        ));
        assert!(matches!(
            parse("+++\nid = 3\ntitle = \"X\"\n+++\n"),
            Err(FrontMatterError::Toml(_))
        ));
    }

    #[test]
    fn a_bad_id_is_refused_and_a_good_one_accepted() {
        for bad in [
            "",
            "-x",
            "Small-Change",
            "small change",
            "small_change",
            "site/x",
            "plugin/x",
            "x/",
            "小改",
            "x.md",
        ] {
            let text = format!("+++\nid = \"{bad}\"\ntitle = \"X\"\n+++\n");
            assert_eq!(
                parse(&text),
                Err(FrontMatterError::BadId(bad.to_string())),
                "{bad:?}"
            );
        }
        for good in [
            "x",
            "0",
            "small-change",
            "a-b-c",
            "issue-development",
            "x--y",
            "a-",
        ] {
            let text = format!("+++\nid = \"{good}\"\ntitle = \"X\"\n+++\n");
            assert_eq!(
                parse(&text).map(|(f, _)| f.id),
                Ok(good.to_string()),
                "{good:?}"
            );
        }
    }

    #[test]
    fn keys_may_come_in_either_order_and_the_title_is_free_text() {
        let (front, _) =
            parse("+++\ntitle = \"投研 / Research (v2)\"\nid = \"investment-research\"\n+++\n")
                .expect("well-formed");
        assert_eq!(front.id, "investment-research");
        assert_eq!(front.title, "投研 / Research (v2)");
    }
}
