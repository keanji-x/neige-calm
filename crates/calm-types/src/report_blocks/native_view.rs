//! Closed, inert native presentation data. Validation grants no execution capability.
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeView {
    pub version: f64,
    pub title: String,
    pub description: String,
    pub snapshot: Snapshot,
    pub rows: Vec<Row>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Snapshot {
    pub id: String,
    pub observed_at: f64,
    pub produced_at: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub layout: Layout,
    pub cells: Vec<Component>,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    One,
    Two,
    Three,
    #[serde(rename = "two-wide-start")]
    TwoWideStart,
    #[serde(rename = "two-wide-end")]
    TwoWideEnd,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Neutral,
    Positive,
    Warning,
    Negative,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Emphasis {
    Primary,
    Normal,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    Prefix,
    Suffix,
}
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "lowercase", deny_unknown_fields)]
pub enum MetricValue {
    Known {
        amount: f64,
        unit: String,
        placement: Placement,
        decimals: f64,
        signed: bool,
    },
    Unknown {
        reason: String,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    pub id: String,
    pub label: String,
    pub value: MetricValue,
    pub detail: String,
    pub tone: Tone,
    pub emphasis: Emphasis,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub label: String,
    pub value: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub label: String,
    pub tone: Tone,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    pub label: String,
    pub body: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub id: String,
    pub label: String,
    pub date: String,
    pub body: String,
    pub note: String,
    pub tone: Tone,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub category: String,
    pub title: String,
    pub summary: String,
    pub status: Status,
    pub handling: Status,
    pub facts: Vec<Field>,
    pub sections: Vec<Section>,
    pub evidence: Vec<Evidence>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordSet {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub items: Vec<Record>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Series {
    pub id: String,
    pub label: String,
    pub palette: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub date: String,
    pub values: Vec<Option<f64>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlotStyle {
    Line,
    Stacked,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dataset {
    pub id: String,
    pub label: String,
    pub unit: String,
    pub style: PlotStyle,
    pub series: Vec<Series>,
    pub points: Vec<Point>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slice {
    pub id: String,
    pub label: String,
    pub value: f64,
    pub palette: f64,
}
#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Component {
    Metrics {
        id: String,
        title: String,
        items: Vec<Metric>,
    },
    TimeSeries {
        id: String,
        title: String,
        caption: String,
        empty_text: String,
        datasets: Vec<Dataset>,
    },
    Distribution {
        id: String,
        title: String,
        unit: String,
        empty_text: String,
        slices: Vec<Slice>,
    },
    Table {
        id: String,
        title: String,
        table: Value,
    },
    Records {
        id: String,
        title: String,
        empty_text: String,
        datasets: Vec<RecordSet>,
    },
}

fn text(value: &str, limit: usize) -> Result<(), String> {
    if value.chars().count() > limit {
        Err(format!("text exceeds {limit} code points"))
    } else {
        Ok(())
    }
}
fn count(value: usize, min: usize, max: usize) -> Result<(), String> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(format!("expected {min}..={max} items"))
    }
}
fn ids<'a>(values: impl Iterator<Item = &'a str>) -> Result<(), String> {
    let mut seen = HashSet::new();
    for value in values {
        if value.is_empty()
            || value.len() > 100
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            || !seen.insert(value)
        {
            return Err("invalid or duplicate identifier".into());
        }
    }
    Ok(())
}
fn number(value: f64) -> Result<(), String> {
    if value.is_finite() && value.abs() <= 1e15 {
        Ok(())
    } else {
        Err("number exceeds native view bounds".into())
    }
}
fn integer(value: f64, min: f64, max: f64) -> Result<(), String> {
    if value.is_finite() && value.fract() == 0.0 && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err("integer outside bounds".into())
    }
}
fn date(value: &str) -> Result<(), String> {
    if !value.starts_with("0000") && super::chart_series::is_valid_ymd(value) {
        Ok(())
    } else {
        Err("invalid UTC calendar date".into())
    }
}
impl Component {
    fn identity(&self) -> (&str, &str) {
        match self {
            Self::Metrics { id, title, .. }
            | Self::TimeSeries { id, title, .. }
            | Self::Distribution { id, title, .. }
            | Self::Table { id, title, .. }
            | Self::Records { id, title, .. } => (id, title),
        }
    }
    fn validate(&self) -> Result<(), String> {
        text(self.identity().1, 200)?;
        match self {
            Self::Metrics { items, .. } => {
                count(items.len(), 1, 8)?;
                ids(items.iter().map(|i| i.id.as_str()))?;
                count(
                    items
                        .iter()
                        .filter(|i| matches!(i.emphasis, Emphasis::Primary))
                        .count(),
                    0,
                    1,
                )?;
                for item in items {
                    text(&item.label, 120)?;
                    text(&item.detail, 500)?;
                    match &item.value {
                        MetricValue::Known {
                            amount,
                            unit,
                            decimals,
                            ..
                        } => {
                            number(*amount)?;
                            text(unit, 32)?;
                            integer(*decimals, 0.0, 4.0)?;
                        }
                        MetricValue::Unknown { reason } => text(reason, 500)?,
                    }
                }
            }
            Self::TimeSeries {
                caption,
                empty_text,
                datasets,
                ..
            } => {
                text(caption, 500)?;
                text(empty_text, 500)?;
                count(datasets.len(), 1, 4)?;
                ids(datasets.iter().map(|d| d.id.as_str()))?;
                for data in datasets {
                    text(&data.label, 120)?;
                    text(&data.unit, 32)?;
                    count(data.series.len(), 1, 6)?;
                    count(data.points.len(), 0, 500)?;
                    ids(data.series.iter().map(|s| s.id.as_str()))?;
                    for series in &data.series {
                        text(&series.label, 120)?;
                        integer(series.palette, 1.0, 7.0)?;
                    }
                    let mut previous: Option<&str> = None;
                    for point in &data.points {
                        date(&point.date)?;
                        if previous.is_some_and(|p| p >= point.date.as_str()) {
                            return Err("dates must increase".into());
                        }
                        previous = Some(&point.date);
                        if point.values.len() != data.series.len() {
                            return Err("point width must match series".into());
                        }
                        for value in &point.values {
                            if matches!(data.style, PlotStyle::Stacked)
                                && value.is_none_or(|v| v < 0.0)
                            {
                                return Err(
                                    "stacked values must be complete and nonnegative".into()
                                );
                            }
                            if let Some(value) = value {
                                number(*value)?;
                            }
                        }
                    }
                }
            }
            Self::Distribution {
                unit,
                empty_text,
                slices,
                ..
            } => {
                text(unit, 32)?;
                text(empty_text, 500)?;
                count(slices.len(), 0, 12)?;
                ids(slices.iter().map(|s| s.id.as_str()))?;
                for slice in slices {
                    text(&slice.label, 120)?;
                    number(slice.value)?;
                    if slice.value < 0.0 {
                        return Err("negative distribution value".into());
                    }
                    integer(slice.palette, 1.0, 7.0)?;
                }
            }
            Self::Table { table, .. } => super::kinds::validate_inline_table_overlay(table)?,
            Self::Records {
                empty_text,
                datasets,
                ..
            } => {
                text(empty_text, 500)?;
                count(datasets.len(), 1, 4)?;
                ids(datasets.iter().map(|d| d.id.as_str()))?;
                for data in datasets {
                    text(&data.label, 120)?;
                    if let Some(description) = &data.description {
                        text(description, 500)?;
                    }
                    count(data.items.len(), 0, 50)?;
                    ids(data.items.iter().map(|i| i.id.as_str()))?;
                    for item in &data.items {
                        text(&item.category, 120)?;
                        text(&item.title, 200)?;
                        text(&item.summary, 2048)?;
                        text(&item.status.label, 120)?;
                        text(&item.handling.label, 120)?;
                        count(item.facts.len(), 0, 12)?;
                        count(item.sections.len(), 0, 8)?;
                        count(item.evidence.len(), 0, 20)?;
                        ids(item.evidence.iter().map(|e| e.id.as_str()))?;
                        for field in &item.facts {
                            text(&field.label, 120)?;
                            text(&field.value, 2048)?;
                        }
                        for section in &item.sections {
                            text(&section.label, 120)?;
                            text(&section.body, 2048)?;
                        }
                        for e in &item.evidence {
                            text(&e.label, 200)?;
                            date(&e.date)?;
                            text(&e.body, 2048)?;
                            text(&e.note, 500)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
pub fn validate(payload: &Value) -> Result<(), String> {
    let view: NativeView =
        serde_json::from_value(payload.clone()).map_err(|e| format!("view: {e}"))?;
    if view.version != 1.0 {
        return Err("view.version: expected 1".into());
    }
    text(&view.title, 200)?;
    text(&view.description, 500)?;
    ids(std::iter::once(view.snapshot.id.as_str()))?;
    integer(view.snapshot.observed_at, 0.0, 253402300799999.0)?;
    integer(view.snapshot.produced_at, 0.0, 253402300799999.0)?;
    count(view.rows.len(), 1, 6)?;
    ids(view.rows.iter().map(|r| r.id.as_str()))?;
    ids(view
        .rows
        .iter()
        .flat_map(|r| r.cells.iter().map(|c| c.identity().0)))?;
    for row in view.rows {
        text(&row.title, 200)?;
        let expected = match row.layout {
            Layout::One => 1,
            Layout::Two | Layout::TwoWideStart | Layout::TwoWideEnd => 2,
            Layout::Three => 3,
        };
        count(row.cells.len(), expected, expected)?;
        for cell in row.cells {
            cell.validate()
                .map_err(|e| format!("row {}: {e}", row.id))?;
        }
    }
    Ok(())
}
