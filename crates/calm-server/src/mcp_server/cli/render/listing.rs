//! `ls` and `find` text. A track-view listing prints `d name` / `- name` (with `-l`, each entry's
//! own update time too); an `area/reports/` listing (#1838) prints report names, or with `-l` a
//! `UPDATED_AT  TAGS  NAME` table; `find` prints one readable `area/reports/` path per line.
//! Times print as `YYYY-MM-DD HH:MM` in the server's local time; `--json` is the tool result as is.

use chrono::{DateTime, Local, TimeZone};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use super::{RenderError, compact, required_str, shape};
use crate::area_reports::REPORTS_DIR;

const TIME_FORMAT: &str = "%Y-%m-%d %H:%M";
/// Printed for an entry without a time and for a report without tags.
const NONE: &str = "—";
/// Width of a rendered [`TIME_FORMAT`] time.
const TIME_WIDTH: usize = "YYYY-MM-DD HH:MM".len();

pub(super) fn ls(
    tool: &str,
    json: bool,
    long: bool,
    reports: bool,
    value: &Value,
) -> Result<String, RenderError> {
    let entries = entries(tool, value)?;
    if json {
        return Ok(compact(value));
    }
    match (reports, long) {
        (false, false) => track_entries(tool, entries, false),
        (false, true) => track_entries(tool, entries, true),
        (true, false) => entries.iter().try_fold(String::new(), |mut out, entry| {
            out.push_str(report_name(tool, entry)?);
            out.push('\n');
            Ok(out)
        }),
        (true, true) => report_table(tool, entries),
    }
}

pub(super) fn find(tool: &str, json: bool, value: &Value) -> Result<String, RenderError> {
    let entries = entries(tool, value)?;
    if json {
        return Ok(compact(value));
    }
    entries.iter().try_fold(String::new(), |mut out, entry| {
        out.push_str(required_str(entry, "path", tool, "entry")?);
        out.push('\n');
        Ok(out)
    })
}

fn entries<'a>(tool: &str, value: &'a Value) -> Result<&'a [Value], RenderError> {
    value.as_array().map(Vec::as_slice).ok_or_else(|| {
        shape(
            format!("{tool} returned non-array structuredContent"),
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
            Some(ms) => ms
                .as_i64()
                .and_then(|ms| Local.timestamp_millis_opt(ms).single())
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

fn report_name<'a>(tool: &str, entry: &'a Value) -> Result<&'a str, RenderError> {
    required_str(entry, "path", tool, "entry")?
        .strip_prefix(REPORTS_DIR)
        .and_then(|rest| rest.strip_prefix('/'))
        .ok_or_else(|| {
            shape(
                format!("{tool} entry path is not under {REPORTS_DIR}/"),
                tool,
                "entry",
                entry,
            )
        })
}

/// `UPDATED_AT  TAGS  NAME`, tags comma-joined (`—` for none); a tag holds no whitespace or `,`.
fn report_table(tool: &str, entries: &[Value]) -> Result<String, RenderError> {
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let updated_at = required_str(entry, "updatedAt", tool, "entry")?;
        let time = DateTime::parse_from_rfc3339(updated_at)
            .map_err(|_| {
                shape(
                    format!("{tool} entry updatedAt is not RFC 3339"),
                    tool,
                    "entry",
                    entry,
                )
            })?
            .format(TIME_FORMAT)
            .to_string();
        let missing_tags = || {
            shape(
                format!("{tool} entry missing string array tags"),
                tool,
                "entry",
                entry,
            )
        };
        let tags = entry
            .get("tags")
            .and_then(Value::as_array)
            .ok_or_else(missing_tags)?
            .iter()
            .map(|tag| tag.as_str().ok_or_else(missing_tags))
            .collect::<Result<Vec<_>, _>>()?;
        let tags = if tags.is_empty() {
            NONE.to_string()
        } else {
            tags.join(",")
        };
        rows.push([time, tags, report_name(tool, entry)?.to_string()]);
    }
    let tags_width = rows
        .iter()
        .map(|row| row[1].width())
        .chain(["TAGS".len()])
        .max()
        .expect("the header is a row");
    let mut out = format!(
        "{}  {}  NAME\n",
        pad("UPDATED_AT", TIME_WIDTH),
        pad("TAGS", tags_width)
    );
    for [time, tags, name] in rows {
        let (time, tags) = (pad(&time, TIME_WIDTH), pad(&tags, tags_width));
        out.push_str(&format!("{time}  {tags}  {name}\n"));
    }
    Ok(out)
}

/// `text` padded with spaces to `width` terminal columns (a CJK character takes 2), so the next
/// column starts at the same column on every row.
fn pad(text: &str, width: usize) -> String {
    let fill = width.saturating_sub(text.width());
    format!("{text}{:fill$}", "")
}

#[cfg(test)]
mod tests {
    use super::super::{Render, render};
    use chrono::{Local, TimeZone};
    use serde_json::{Value, json};

    fn ls(long: bool, reports: bool, value: &Value) -> String {
        render(Render::Ls { long, reports }, "neige.track.ls", false, value).unwrap()
    }

    fn local(ms: i64) -> chrono::DateTime<Local> {
        Local.timestamp_millis_opt(ms).single().unwrap()
    }

    #[test]
    fn ls_entry_without_kind_is_a_render_error() {
        let entries = json!([
            { "name": "cards/", "kind": "dir" }, { "name": "track.json", "kind": "file" }
        ]);
        assert_eq!(ls(false, false, &entries), "d cards/\n- track.json\n");
        let err = render(
            Render::Ls {
                long: false,
                reports: false,
            },
            "neige.track.ls",
            false,
            &json!([{ "name": "x" }]),
        )
        .unwrap_err();
        assert_eq!(err.message, "neige.track.ls entry missing string kind");
    }

    #[test]
    fn long_track_entries_print_their_own_time_or_a_dash() {
        let at = 1_790_000_000_000;
        let entries = json!([
            { "name": "report.md", "kind": "file", "updated_at": at },
            { "name": "cards/", "kind": "dir" }
        ]);
        assert_eq!(
            ls(true, false, &entries),
            format!(
                "- {}  report.md\nd —                 cards/\n",
                local(at).format("%Y-%m-%d %H:%M")
            )
        );
    }

    fn reports() -> Value {
        let at = |ms: i64| local(ms).to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        json!([
            { "path": "area/reports/认证 方案.md", "title": "认证 方案", "trackId": "t1",
              "tags": ["认证", "架构"], "updatedAt": at(1_790_000_000_000) },
            { "path": "area/reports/x%2Fy~abcdef12.md", "title": "x/y", "trackId": "abcdef1234",
              "tags": [], "updatedAt": at(1_789_000_000_000) }
        ])
    }

    /// Mixed CJK/ASCII tags: every row's NAME starts at the header's NAME display column.
    #[test]
    fn long_report_table_aligns_the_name_column_by_display_width() {
        use unicode_width::UnicodeWidthStr;
        let at = local(1_790_000_000_000).to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
        let entry = |name: &str, tags: Value| {
            json!({ "path": format!("area/reports/{name}"), "title": name, "trackId": "t",
                    "tags": tags, "updatedAt": at })
        };
        let table = ls(
            true,
            true,
            &json!([
                entry("认证 方案.md", json!(["认证", "架构"])),
                entry("login.md", json!(["auth", "x"])),
                entry("登录 排查~abcdef12.md", json!(["排障"])),
                entry("none.md", json!([])),
            ]),
        );
        let columns: Vec<usize> = table
            .lines()
            .zip([
                "NAME",
                "认证 方案.md",
                "login.md",
                "登录 排查~abcdef12.md",
                "none.md",
            ])
            .map(|(line, name)| {
                let prefix = line.strip_suffix(name).unwrap_or_else(|| panic!("{table}"));
                prefix.width()
            })
            .collect();
        assert_eq!(columns.len(), 5, "{table}");
        assert!(
            columns.iter().all(|c| *c == columns[0]),
            "{columns:?}\n{table}"
        );
    }

    #[test]
    fn report_listings_print_names_a_table_or_paths() {
        assert_eq!(
            ls(false, true, &reports()),
            "认证 方案.md\nx%2Fy~abcdef12.md\n"
        );
        let time = |ms| local(ms).format("%Y-%m-%d %H:%M").to_string();
        assert_eq!(
            ls(true, true, &reports()),
            format!(
                "UPDATED_AT        TAGS       NAME\n{}  认证,架构  认证 方案.md\n{}  —          x%2Fy~abcdef12.md\n",
                time(1_790_000_000_000),
                time(1_789_000_000_000)
            )
        );
        assert_eq!(
            ls(true, true, &json!([])),
            "UPDATED_AT        TAGS  NAME\n",
            "an empty directory still prints its header"
        );
        assert_eq!(
            render(Render::Find, "neige.report.find", false, &reports()).unwrap(),
            "area/reports/认证 方案.md\narea/reports/x%2Fy~abcdef12.md\n"
        );
        for how in [
            Render::Find,
            Render::Ls {
                long: true,
                reports: true,
            },
        ] {
            assert_eq!(
                render(how, "t", true, &reports()).unwrap(),
                format!("{}\n", reports())
            );
        }
    }

    #[test]
    fn report_entry_shape_errors_name_the_missing_field() {
        let mut bad = reports();
        bad[0]["updatedAt"] = json!("yesterday");
        let err = render(
            Render::Ls {
                long: true,
                reports: true,
            },
            "neige.track.ls",
            false,
            &bad,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "neige.track.ls entry updatedAt is not RFC 3339"
        );
        let mut bad = reports();
        bad[1]["path"] = json!("report.md");
        let err = render(
            Render::Ls {
                long: false,
                reports: true,
            },
            "neige.track.ls",
            false,
            &bad,
        )
        .unwrap_err();
        assert_eq!(
            err.message,
            "neige.track.ls entry path is not under area/reports/"
        );
    }
}
