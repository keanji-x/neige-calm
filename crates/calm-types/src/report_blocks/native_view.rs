//! Closed, inert native presentation data. Validation grants no execution capability.
mod model;
pub use model::*;
use serde_json::Value;
use std::collections::HashSet;

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
    /// The cell kind; exhaustive, so a new variant cannot compile without one.
    pub fn kind(&self) -> ComponentKind {
        match self {
            Self::Metrics { .. } => ComponentKind::Metrics,
            Self::TimeSeries { .. } => ComponentKind::TimeSeries,
            Self::Distribution { .. } => ComponentKind::Distribution,
            Self::Table { .. } => ComponentKind::Table,
            Self::Bars { .. } => ComponentKind::Bars,
            Self::Meter { .. } => ComponentKind::Meter,
            Self::Records { .. } => ComponentKind::Records,
        }
    }
    fn identity(&self) -> (&str, &str) {
        match self {
            Self::Metrics { id, title, .. }
            | Self::TimeSeries { id, title, .. }
            | Self::Distribution { id, title, .. }
            | Self::Table { id, title, .. }
            | Self::Bars { id, title, .. }
            | Self::Meter { id, title, .. }
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
                        MetricValue::Text { text: value } => text(value, 2048)?,
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
                                && value.is_none_or(|v| v.0 < 0.0)
                            {
                                return Err(
                                    "stacked values must be complete and nonnegative".into()
                                );
                            }
                            if let Some(value) = value {
                                number(value.0)?;
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
            Self::Bars {
                unit,
                empty_text,
                points,
                ..
            } => {
                text(unit, 32)?;
                text(empty_text, 500)?;
                count(points.len(), 0, 24)?;
                for point in points {
                    text(&point.label, 120)?;
                    number(point.value)?;
                }
            }
            Self::Meter {
                unit,
                detail,
                used,
                limit,
                used_label,
                limit_label,
                empty_text,
                ..
            } => {
                text(unit, 32)?;
                text(detail, 500)?;
                text(used_label, 120)?;
                text(limit_label, 120)?;
                text(empty_text, 500)?;
                if let Some(used) = used {
                    number(*used)?;
                    if *used < 0.0 {
                        return Err("negative meter used".into());
                    }
                }
                if let Some(limit) = limit {
                    number(*limit)?;
                    if *limit <= 0.0 {
                        return Err("meter limit must be positive".into());
                    }
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
                    count(data.items.len(), 0, 100)?;
                    ids(data.items.iter().map(|i| i.id.as_str()))?;
                    for item in &data.items {
                        text(&item.subtitle, 120)?;
                        text(&item.title, 200)?;
                        text(&item.summary, 8000)?;
                        count(item.badges.len(), 0, 12)?;
                        for badge in &item.badges {
                            text(&badge.label, 120)?;
                            text(&badge.value, 2048)?;
                        }
                        count(item.facts.len(), 0, 12)?;
                        count(item.sections.len(), 0, 8)?;
                        count(item.disclosures.len(), 0, 20)?;
                        ids(item.disclosures.iter().map(|e| e.id.as_str()))?;
                        for field in &item.facts {
                            text(&field.label, 120)?;
                            text(&field.value, 2048)?;
                        }
                        for section in &item.sections {
                            text(&section.label, 120)?;
                            text(&section.body, 8000)?;
                        }
                        for e in &item.disclosures {
                            text(&e.label, 200)?;
                            text(&e.body, 8000)?;
                            text(&e.note, 500)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
impl RowCell {
    fn id(&self) -> &str {
        match self {
            Self::Live(slot) => &slot.id,
            Self::Inline(component) => component.identity().0,
        }
    }
}
fn validate_snapshot(snapshot: &Snapshot) -> Result<(), String> {
    ids(std::iter::once(snapshot.id.as_str()))?;
    for time in [snapshot.observed_at, snapshot.produced_at]
        .into_iter()
        .flatten()
    {
        integer(time, 0.0, 253402300799999.0)?;
    }
    Ok(())
}
fn validate_slot(slot: &LiveSlot) -> Result<(), String> {
    text(&slot.source, super::kinds::MAX_STRING_CHARS)?;
    super::kinds::validate_live_source(&slot.source)
}
pub fn validate(payload: &Value) -> Result<(), String> {
    let view: NativeView =
        serde_json::from_value(payload.clone()).map_err(|e| format!("view: {e}"))?;
    if view.version != 1.0 {
        return Err("view.version: expected 1".into());
    }
    text(&view.title, 200)?;
    text(&view.description, 500)?;
    let inline = view
        .rows
        .iter()
        .flat_map(|r| &r.cells)
        .any(|c| matches!(c, RowCell::Inline(_)));
    match (&view.snapshot, inline) {
        (Some(snapshot), true) => validate_snapshot(snapshot)?,
        (None, false) => {}
        (None, true) => {
            return Err("view.snapshot: required while the view has an inline cell".into());
        }
        (Some(_), false) => {
            return Err("view.snapshot: must be null when every cell is a live slot".into());
        }
    }
    count(view.rows.len(), 1, 6)?;
    ids(view.rows.iter().map(|r| r.id.as_str()))?;
    ids(view
        .rows
        .iter()
        .flat_map(|r| r.cells.iter().map(RowCell::id)))?;
    for row in view.rows {
        text(&row.title, 200)?;
        let expected = match row.layout {
            Layout::One => 1,
            Layout::Two | Layout::TwoWideStart | Layout::TwoWideEnd => 2,
            Layout::Three => 3,
        };
        count(row.cells.len(), expected, expected)?;
        for cell in row.cells {
            match cell {
                RowCell::Live(slot) => validate_slot(&slot),
                RowCell::Inline(component) => component.validate(),
            }
            .map_err(|e| format!("row {}: {e}", row.id))?;
        }
    }
    Ok(())
}

/// Read-side check of one live slot's overlay payload: size cap, envelope, the
/// template's expected kind, then the cell itself.
pub fn validate_unit(expects: ComponentKind, payload: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(payload).map_err(|e| format!("unit: {e}"))?;
    if bytes.len() > super::kinds::MAX_LIVE_VIEW_BYTES {
        return Err(format!(
            "unit: {} bytes exceeds the {} byte limit",
            bytes.len(),
            super::kinds::MAX_LIVE_VIEW_BYTES
        ));
    }
    let unit: DataUnit =
        serde_json::from_value(payload.clone()).map_err(|e| format!("unit: {e}"))?;
    validate_snapshot(&unit.snapshot).map_err(|e| format!("unit.snapshot: {e}"))?;
    let kind = unit.cell.kind();
    if kind != expects {
        return Err(format!(
            "unit.cell: kind {kind:?} does not match the slot's expected {expects:?}"
        ));
    }
    ids(std::iter::once(unit.cell.identity().0))?;
    unit.cell.validate().map_err(|e| format!("unit.cell: {e}"))
}

/// Checked-in contract used by backend discovery; regenerate from the DTOs.
pub fn schema() -> Value {
    serde_json::from_str(include_str!("native_view.schema.json"))
        .expect("generated native view schema")
}

/// Structural JSON Schema derived from the authoritative Rust presentation DTOs.
pub fn generated_schema() -> Value {
    use utoipa::{PartialSchema, ToSchema};
    let mut definitions = Vec::new();
    NativeView::schemas(&mut definitions);
    definitions.push((NativeView::name().into(), NativeView::schema()));
    DataUnit::schemas(&mut definitions);
    definitions.push((DataUnit::name().into(), DataUnit::schema()));
    let mut schema = serde_json::to_value(NativeView::schema()).unwrap();
    let definitions: serde_json::Map<String, Value> = definitions
        .into_iter()
        .map(|(name, schema)| (name, serde_json::to_value(schema).unwrap()))
        .collect();
    schema["$defs"] = Value::Object(definitions);
    fn references(value: &mut Value) {
        match value {
            Value::Object(map) => {
                if let Some(Value::String(reference)) = map.get_mut("$ref") {
                    *reference = reference.replace("#/components/schemas/", "#/$defs/");
                }
                // utoipa closes structs but does not propagate serde's deny_unknown_fields
                // onto internally tagged enum branches. These object variants are closed too.
                if map.get("type").and_then(Value::as_str) == Some("object")
                    && map.contains_key("properties")
                {
                    map.insert("additionalProperties".into(), Value::Bool(false));
                }
                for value in map.values_mut() {
                    references(value);
                }
            }
            Value::Array(values) => {
                for value in values {
                    references(value);
                }
            }
            _ => {}
        }
    }
    references(&mut schema);
    schema["$schema"] = Value::String("https://json-schema.org/draft/2020-12/schema".into());
    schema
}

/// Import-free TypeScript declarations for every structural DTO.
pub fn typescript() -> String {
    use ts_rs::TS;
    let config = ts_rs::Config::default();
    let declarations = [
        NativeView::decl(&config),
        Snapshot::decl(&config),
        Row::decl(&config),
        RowCell::decl(&config),
        LiveSlot::decl(&config),
        LiveTag::decl(&config),
        ComponentKind::decl(&config),
        DataUnit::decl(&config),
        Layout::decl(&config),
        Tone::decl(&config),
        Emphasis::decl(&config),
        Placement::decl(&config),
        MetricValue::decl(&config),
        Metric::decl(&config),
        Field::decl(&config),
        Badge::decl(&config),
        Section::decl(&config),
        Disclosure::decl(&config),
        ViewRecord::decl(&config),
        RecordSet::decl(&config),
        Series::decl(&config),
        Point::decl(&config),
        ObservationValue::decl(&config),
        PlotStyle::decl(&config),
        Dataset::decl(&config),
        Slice::decl(&config),
        Component::decl(&config),
        BarPoint::decl(&config),
        InlineTable::decl(&config),
        TableColumn::decl(&config),
        TableAlign::decl(&config),
        TableScalar::decl(&config),
        TableText::decl(&config),
        TableNull::decl(&config),
    ];
    declarations
        .into_iter()
        .map(|declaration| format!("export {declaration}\n"))
        .collect()
}
