//! Complete US earnings calendar windows, with auditable raw HTTP pages.
//!
//! `count` bounds raw offsets, not published event cardinality: the service
//! normalizes fiscal variants and report aliases into company/date entries.
//! Walk through that stable bound even across short, repeated, or empty pages.
//! Advance only by visible length (at least one), conservatively overlapping
//! raw positions instead of assuming a requested page size was consumed.
//! Coverage refers to upstream group labels, not inferred NY earnings dates.
use super::{Fetched, LongbridgeSource, invalid_body};
use crate::{Endpoint, types::Response};
use chrono::{Duration, NaiveDate};
use serde_json::{Value, json};
use std::collections::BTreeMap;
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 2048;
const MAX_RAW_BYTES: usize = 8 * 1024 * 1024;

impl LongbridgeSource {
    pub(super) async fn market_calendar(
        &self,
        endpoint: &Endpoint,
        query: &[(String, String)],
        may_send: &(impl Fn() -> bool + Sync),
    ) -> Option<Fetched> {
        let arg = |name| {
            query
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        };
        let parse = |s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
        let (Some(start), Some(end)) =
            (arg("date").and_then(parse), arg("date_end").and_then(parse))
        else {
            return Some(invalid_body("invalid_calendar_range"));
        };
        if start > end || (end - start).num_days() > 60 {
            return Some(invalid_body("invalid_calendar_range"));
        }
        let mut pages = Vec::new();
        let mut days = BTreeMap::<String, BTreeMap<String, BTreeMap<String, Value>>>::new();
        let mut reported_counts = BTreeMap::new();
        let mut size = 0;
        let mut day = start;
        'dates: while day <= end {
            let day_string = day.to_string();
            let mut pager = DayPager::default();
            loop {
                if pages.len() >= MAX_PAGES {
                    return Some(invalid_body("calendar_page_limit"));
                }
                let page_offset = pager.offset;
                let mut page_query = query.to_vec();
                for (name, value) in &mut page_query {
                    if name == "date" {
                        *value = day_string.clone();
                    }
                    if name == "count" {
                        *value = PAGE_SIZE.to_string();
                    }
                }
                page_query.push(("offset".into(), pager.offset.to_string()));
                let fetched = self.http_page(endpoint, &page_query, may_send).await?;
                let Response::Body(raw) = fetched.response() else {
                    return Some(fetched);
                };
                size += raw.len();
                if size > MAX_RAW_BYTES {
                    return Some(invalid_body("calendar_byte_limit"));
                }
                let body: Value = match serde_json::from_slice(raw) {
                    Ok(body) => body,
                    Err(_) => return Some(invalid_body("invalid_calendar_json")),
                };
                let (done, events) = match pager.advance(&day_string, &body) {
                    Ok(result) => result,
                    Err(code) => return Some(invalid_body(code)),
                };
                if let Some(total) = pager.total {
                    reported_counts.insert(day_string.clone(), total);
                }
                let date_events = days.entry(day_string.clone()).or_default();
                for event in events {
                    let (Some(symbol), Some(id)) = (event["symbol"].as_str(), event["id"].as_str())
                    else {
                        return Some(invalid_body("invalid_calendar_event"));
                    };
                    if event["market"] != "US" || symbol.is_empty() || id.is_empty() {
                        return Some(invalid_body("invalid_calendar_event"));
                    }
                    let records = date_events.entry(symbol.to_owned()).or_default();
                    if records.get(id).is_some_and(|old| old != &event) {
                        return Some(invalid_body("calendar_changed_during_capture"));
                    }
                    records.insert(id.to_owned(), event);
                }
                // Keep raw data bytes inside a JSON string: decoding raw_json
                // reproduces exactly the SDK HTTP data body, including numbers.
                pages.push(
                    json!({"date":day_string,"offset":page_offset,"count":PAGE_SIZE,
                    "fetched_at":chrono::Utc::now().to_rfc3339(),
                    "raw_json":std::str::from_utf8(raw).expect("SDK returned UTF-8")}),
                );
                if body["list"].as_array().is_some_and(|g| g.is_empty())
                    && body["next_date"] == ""
                    && pager.total.is_none()
                {
                    while day < end {
                        day = day
                            .checked_add_signed(Duration::days(1))
                            .expect("bounded date");
                        days.entry(day.to_string()).or_default();
                    }
                    break 'dates;
                }
                if done {
                    break;
                }
            }
            day = day
                .checked_add_signed(Duration::days(1))
                .expect("bounded calendar date");
        }
        let company_counts: BTreeMap<_, _> = days
            .iter()
            .map(|(date, companies)| (date, companies.len()))
            .collect();
        Some(Fetched::body(
            json!({"format":"earnings_calendar_v1","market":"US",
            "coverage":"upstream_group_company_dates","reported_counts":reported_counts,"company_counts":company_counts,
            "start":start.to_string(),"end":end.to_string(),"days":days,"pages":pages})
            .to_string(),
        ))
    }
}

#[derive(Default)]
struct DayPager {
    offset: usize,
    total: Option<usize>,
}
impl DayPager {
    fn advance(&mut self, day: &str, body: &Value) -> Result<(bool, Vec<Value>), &'static str> {
        let groups = body["list"].as_array().ok_or("invalid_calendar_groups")?;
        if !body
            .get("next_date")
            .is_some_and(|v| v.is_null() || v.is_string())
        {
            return Err("invalid_calendar_cursor");
        }
        let mut events = Vec::new();
        let mut found = false;
        for group in groups {
            let date = group["date"].as_str().ok_or("invalid_calendar_date")?;
            let parsed =
                NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| "invalid_calendar_date")?;
            if parsed.to_string() != date || date < day {
                return Err("invalid_calendar_date");
            }
            let infos = group["infos"].as_array().ok_or("invalid_calendar_infos")?;
            if date != day {
                continue;
            }
            if found {
                return Err("duplicate_calendar_group");
            }
            found = true;
            let total = group["count"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or("invalid_calendar_count")?;
            if total > 100_000 || infos.len() > PAGE_SIZE || infos.len() > total {
                return Err("invalid_calendar_count");
            }
            if self.total.is_some_and(|old| old != total) {
                return Err("calendar_changed_during_capture");
            }
            self.total = Some(total);
            events.extend(infos.iter().cloned());
        }
        // A displayed row represents at least one raw offset position. A
        // shorter stride creates safe overlap when the provider coalesces rows
        // or caps a page. Identical normalized pages can be legitimate.
        self.offset += events.len().max(1);
        let done = self.total.is_none_or(|total| self.offset >= total);
        Ok((done, events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_page_cannot_skip_a_later_company() {
        let first = json!({"list":[{"date":"2026-10-14","count":3,"infos":[{"id":"a"}]}],"next_date":"2026-10-15"});
        let last = json!({"list":[{"date":"2026-10-14","count":3,"infos":[{"id":"b"}]}],"next_date":"2026-10-15"});
        let mut p = DayPager::default();
        assert!(!p.advance("2026-10-14", &first).unwrap().0);
        assert!(!p.advance("2026-10-14", &first).unwrap().0);
        assert!(p.advance("2026-10-14", &last).unwrap().0);
    }

    #[test]
    fn empty_raw_positions_do_not_invent_missing_events() {
        let first = json!({"list":[{"date":"2026-10-14","count":2,"infos":[{"id":"a"}]}],"next_date":"2026-10-15"});
        for last in [
            json!({"list":[],"next_date":""}),
            json!({"list":[{"date":"2026-10-15","count":1,"infos":[{"id":"b"}]}],"next_date":"2026-10-16"}),
            json!({"list":[{"date":"2026-10-14","count":2,"infos":[]}],"next_date":""}),
        ] {
            let mut p = DayPager::default();
            assert!(!p.advance("2026-10-14", &first).unwrap().0);
            let (done, events) = p.advance("2026-10-14", &last).unwrap();
            assert!(done);
            assert!(events.is_empty());
            assert_eq!(p.offset, 2);
        }
    }

    #[test]
    fn changing_or_invalid_offset_bounds_fail_closed() {
        let first = json!({"list":[{"date":"2026-10-14","count":22,"infos":[{"id":"a"}]}],"next_date":"2026-10-15"});
        let mut p = DayPager::default();
        assert!(!p.advance("2026-10-14", &first).unwrap().0);
        let changed = json!({"list":[{"date":"2026-10-14","count":23,"infos":[]}],"next_date":""});
        assert_eq!(
            p.advance("2026-10-14", &changed).unwrap_err(),
            "calendar_changed_during_capture"
        );
        let invalid =
            json!({"list":[{"date":"2026-10-14","count":100001,"infos":[]}],"next_date":""});
        assert_eq!(
            DayPager::default()
                .advance("2026-10-14", &invalid)
                .unwrap_err(),
            "invalid_calendar_count"
        );
    }

    #[test]
    fn normalized_pages_overlap_without_matching_record_cardinality() {
        let first = json!({"list":[{"date":"2026-10-14","count":4,"infos":[{"id":"a"}]}],"next_date":"2026-10-15"});
        let empty = json!({"list":[],"next_date":"2026-10-15"});
        let last = json!({"list":[{"date":"2026-10-14","count":4,"infos":[{"id":"b"}]}],"next_date":"2026-10-15"});
        let mut p = DayPager::default();
        for (i, page) in [&first, &first, &empty, &last].into_iter().enumerate() {
            let (done, _) = p
                .advance("2026-10-14", page)
                .expect("normalized overlaps and empty raw positions are valid");
            assert_eq!(done, i == 3);
            assert_eq!(p.offset, i + 1);
        }
    }
}
