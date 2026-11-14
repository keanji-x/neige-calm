//! List bound view rows or canonical named directory rows; find prints Report paths.

use chrono::DateTime;
use serde_json::Value;

use super::{RenderError, compact, required_str, shape};

const TIME_FORMAT: &str = "%Y-%m-%d %H:%M";
/// Printed for an entry without a time and for a report without tags.
const NONE: &str = "—";
/// Width of a rendered [`TIME_FORMAT`] time.
const TIME_WIDTH: usize = "YYYY-MM-DD HH:MM".len();

pub(super) fn ls(tool: &str, json: bool, long: bool, value: &Value) -> Result<String, RenderError> {
    let entries = rows(tool, value, "entries")?;
    if json {
        return Ok(compact(value));
    }
    if let Some(pattern) = value["date_pattern"].as_str() {
        return Ok(format!("Date files: {pattern}\n"));
    }
    if entries.first().is_some_and(|row| row.get("id").is_some()) {
        let mut out = String::new();
        for entry in entries {
            out.push_str(&format!(
                "{}  {}  {}\n",
                required_str(entry, "id", tool, "entry")?,
                required_str(entry, "title", tool, "entry")?,
                required_str(entry, "path", tool, "entry")?
            ));
        }
        if let Some(cursor) = value["next_cursor"].as_str() {
            out.push_str(&format!("next_cursor: {cursor}\n"));
        }
        return Ok(out);
    }
    track_entries(tool, entries, long)
}

pub(super) fn find(tool: &str, json: bool, value: &Value) -> Result<String, RenderError> {
    let entries = rows(tool, value, "reports")?;
    if json {
        return Ok(compact(value));
    }
    entries.iter().try_fold(String::new(), |mut out, entry| {
        out.push_str(required_str(entry, "path", tool, "entry")?);
        out.push('\n');
        Ok(out)
    })
}

/// Rows under the declared result envelope key.
fn rows<'a>(tool: &str, value: &'a Value, key: &str) -> Result<&'a [Value], RenderError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| {
            shape(
                format!("{tool} returned no `{key}` array"),
                tool,
                "value",
                value,
            )
        })
}

/// `d name`, or with `long` `d YYYY-MM-DD HH:MM  name` (`—` for an entry without a time).
fn track_entries(tool: &str, entries: &[Value], long: bool) -> Result<String, RenderError> {
    let mut out = String::new();
    for entry in entries {
        let name = required_str(entry, "name", tool, "entry")?;
        let kind = required_str(entry, "kind", tool, "entry")?;
        let prefix = if kind == "dir" { 'd' } else { '-' };
        if !long {
            out.push_str(&format!("{prefix} {name}\n"));
            continue;
        }
        let time = match entry.get("updated_at") {
            None | Some(Value::Null) => NONE.to_string(),
            Some(at) => at
                .as_str()
                .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                .map(|at| at.format(TIME_FORMAT).to_string())
                .ok_or_else(|| {
                    shape(
                        format!("{tool} entry has a non-timestamp updated_at"),
                        tool,
                        "entry",
                        entry,
                    )
                })?,
        };
        out.push_str(&format!("{prefix} {time:<TIME_WIDTH$}  {name}\n"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::{Render, render};
    use chrono::{Local, TimeZone};
    use serde_json::{Value, json};

    fn ls(long: bool, value: &Value) -> String {
        render(Render::Ls { long }, "neige_ls", false, value).unwrap()
    }

    fn local(ms: i64) -> chrono::DateTime<Local> {
        Local.timestamp_millis_opt(ms).single().unwrap()
    }

    #[test]
    fn ls_entry_without_kind_is_a_render_error() {
        let entries = json!({ "entries": [
            { "name": "cards/", "kind": "dir" }, { "name": "track.json", "kind": "file" }
        ] });
        assert_eq!(ls(false, &entries), "d cards/\n- track.json\n");
        let err = render(
            Render::Ls { long: false },
            "neige_ls",
            false,
            &json!({ "entries": [{ "name": "x" }] }),
        )
        .unwrap_err();
        assert_eq!(err.message, "neige_ls entry missing string kind");
    }

    #[test]
    fn long_track_entries_print_their_own_time_or_a_dash() {
        let at = 1_790_000_000_000;
        let rfc3339 = local(at).to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        let entries = json!({ "entries": [
            { "name": "report.md", "kind": "file", "updated_at": rfc3339 },
            { "name": "cards/", "kind": "dir", "updated_at": null }
        ] });
        assert_eq!(
            ls(true, &entries),
            format!(
                "- {}  report.md\nd —                 cards/\n",
                local(at).format("%Y-%m-%d %H:%M")
            )
        );
        let unix_ms = json!({ "entries": [{ "name": "x", "kind": "file", "updated_at": at }] });
        assert!(render(Render::Ls { long: true }, "neige_ls", false, &unix_ms).is_err());
    }

    #[test]
    fn named_directory_entries_print_id_title_and_canonical_path() {
        let entries = json!({"entries":[{"id":"track-id","title":"认证 方案","path":"/研发/认证 方案/","kind":"dir"}],"next_cursor":"opaque"});
        assert_eq!(
            ls(false, &entries),
            "track-id  认证 方案  /研发/认证 方案/\nnext_cursor: opaque\n"
        );
        assert_eq!(ls(true, &entries), ls(false, &entries));
        let reports = json!({"reports":[{"path":"/研发/认证 方案/report.md"}]});
        assert_eq!(
            render(Render::Find, "neige_report_find", false, &reports).unwrap(),
            "/研发/认证 方案/report.md\n"
        );
    }
    #[test]
    fn named_directory_shape_errors_refuse_missing_fields() {
        for field in ["title", "path"] {
            let mut row = json!({"id":"t","title":"T","path":"/A/T/","kind":"dir"});
            row.as_object_mut().unwrap().remove(field);
            assert!(
                render(
                    Render::Ls { long: false },
                    "neige_ls",
                    false,
                    &json!({"entries":[row]})
                )
                .is_err()
            );
        }
        let bad = json!({"entries":[{"name":"x","kind":"file","updated_at":"yesterday"}]});
        assert!(render(Render::Ls { long: true }, "neige_ls", false, &bad).is_err());
        assert!(
            render(
                Render::Find,
                "neige_report_find",
                false,
                &json!({"reports":[{}]})
            )
            .is_err()
        );
    }
}
