//! Authoritative, domain-free presentation DTOs.
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use ts_rs::TS;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct NativeView {
    #[schema(value_type = i64, minimum = 1, maximum = 1)]
    pub version: f64,
    #[schema(max_length = 200)]
    pub title: String,
    #[schema(max_length = 500)]
    pub description: String,
    pub snapshot: Snapshot,
    #[schema(min_items = 1, max_items = 6)]
    pub rows: Vec<Row>,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Snapshot {
    #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
    pub id: String,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required = true, value_type = Option<i64>, minimum = 0, maximum = 253402300799999.0)]
    pub observed_at: Option<f64>,
    #[serde(deserialize_with = "required_nullable")]
    #[schema(required = true, value_type = Option<i64>, minimum = 0, maximum = 253402300799999.0)]
    pub produced_at: Option<f64>,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Row {
    #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
    pub id: String,
    #[schema(max_length = 200)]
    pub title: String,
    pub layout: Layout,
    #[schema(min_items = 1, max_items = 3)]
    pub cells: Vec<Component>,
}
#[derive(Deserialize, ToSchema, TS)]
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
#[derive(Deserialize, ToSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Neutral,
    Positive,
    Warning,
    Negative,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum Emphasis {
    Primary,
    Normal,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    Prefix,
    Suffix,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(tag = "state", rename_all = "lowercase", deny_unknown_fields)]
pub enum MetricValue {
    Known {
        #[schema(minimum = -1000000000000000.0, maximum = 1000000000000000.0)]
        amount: f64,
        #[schema(max_length = 32)]
        unit: String,
        placement: Placement,
        #[schema(value_type = i64, minimum = 0, maximum = 4)]
        decimals: f64,
        signed: bool,
    },
    Text {
        #[schema(max_length = 2048)]
        text: String,
    },
    Unknown {
        #[schema(max_length = 500)]
        reason: String,
    },
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
    pub id: String,
    #[schema(max_length = 120)]
    pub label: String,
    pub value: MetricValue,
    #[schema(max_length = 500)]
    pub detail: String,
    pub tone: Tone,
    pub emphasis: Emphasis,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Field {
    #[schema(max_length = 120)]
    pub label: String,
    #[schema(max_length = 2048)]
    pub value: String,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Badge {
    #[schema(max_length = 120)]
    pub label: String,
    #[schema(max_length = 2048)]
    pub value: String,
    pub tone: Tone,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Section {
    #[schema(max_length = 120)]
    pub label: String,
    #[schema(max_length = 8000)]
    pub body: String,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Disclosure {
    #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
    pub id: String,
    #[schema(max_length = 200)]
    pub label: String,
    #[schema(max_length = 8000)]
    pub body: String,
    #[schema(max_length = 500)]
    pub note: String,
    pub tone: Tone,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct ViewRecord {
    #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
    pub id: String,
    #[schema(max_length = 120)]
    pub subtitle: String,
    #[schema(max_length = 200)]
    pub title: String,
    #[schema(max_length = 8000)]
    pub summary: String,
    #[schema(min_items = 0, max_items = 12)]
    pub badges: Vec<Badge>,
    #[schema(min_items = 0, max_items = 12)]
    pub facts: Vec<Field>,
    #[schema(min_items = 0, max_items = 8)]
    pub sections: Vec<Section>,
    #[schema(min_items = 0, max_items = 20)]
    pub disclosures: Vec<Disclosure>,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct RecordSet {
    #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
    pub id: String,
    #[schema(max_length = 120)]
    pub label: String,
    #[schema(max_length = 500)]
    #[ts(optional = nullable)]
    pub description: Option<String>,
    #[schema(min_items = 0, max_items = 100)]
    pub items: Vec<ViewRecord>,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Series {
    #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
    pub id: String,
    #[schema(max_length = 120)]
    pub label: String,
    #[schema(value_type = i64, minimum = 1, maximum = 7)]
    pub palette: f64,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Point {
    #[schema(
        min_length = 10,
        max_length = 10,
        pattern = "^[0-9]{4}-[0-9]{2}-[0-9]{2}$"
    )]
    pub date: String,
    #[schema(min_items = 1, max_items = 6)]
    pub values: Vec<Option<f64>>,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum PlotStyle {
    Line,
    Stacked,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Dataset {
    #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
    pub id: String,
    #[schema(max_length = 120)]
    pub label: String,
    #[schema(max_length = 32)]
    pub unit: String,
    pub style: PlotStyle,
    #[schema(min_items = 1, max_items = 6)]
    pub series: Vec<Series>,
    #[schema(min_items = 0, max_items = 500)]
    pub points: Vec<Point>,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct Slice {
    #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
    pub id: String,
    #[schema(max_length = 120)]
    pub label: String,
    #[schema(minimum = 0, maximum = 1000000000000000.0)]
    pub value: f64,
    #[schema(value_type = i64, minimum = 1, maximum = 7)]
    pub palette: f64,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Component {
    Metrics {
        #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
        id: String,
        #[schema(max_length = 200)]
        title: String,
        #[schema(min_items = 1, max_items = 8)]
        items: Vec<Metric>,
    },
    TimeSeries {
        #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
        id: String,
        #[schema(max_length = 200)]
        title: String,
        #[schema(max_length = 500)]
        caption: String,
        #[schema(max_length = 500)]
        #[serde(rename = "emptyText")]
        empty_text: String,
        #[schema(min_items = 1, max_items = 4)]
        datasets: Vec<Dataset>,
    },
    Distribution {
        #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
        id: String,
        #[schema(max_length = 200)]
        title: String,
        #[schema(max_length = 32)]
        unit: String,
        #[schema(max_length = 500)]
        #[serde(rename = "emptyText")]
        empty_text: String,
        #[schema(min_items = 0, max_items = 12)]
        slices: Vec<Slice>,
    },
    Table {
        #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
        id: String,
        #[schema(max_length = 200)]
        title: String,
        #[schema(value_type = InlineTable)]
        #[ts(type = "InlineTable")]
        table: Value,
    },
    Bars {
        #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
        id: String,
        #[schema(max_length = 200)]
        title: String,
        #[schema(max_length = 32)]
        unit: String,
        #[schema(max_length = 500)]
        #[serde(rename = "emptyText")]
        empty_text: String,
        #[schema(min_items = 0, max_items = 24)]
        points: Vec<BarPoint>,
    },
    Meter {
        #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
        id: String,
        #[schema(max_length = 200)]
        title: String,
        #[schema(max_length = 32)]
        unit: String,
        #[schema(max_length = 500)]
        detail: String,
        #[serde(deserialize_with = "required_nullable")]
        #[schema(required = true, minimum = 0, maximum = 1000000000000000.0)]
        used: Option<f64>,
        #[serde(deserialize_with = "required_nullable")]
        #[schema(required = true, exclusive_minimum = 0, maximum = 1000000000000000.0)]
        limit: Option<f64>,
        #[schema(max_length = 120)]
        #[serde(rename = "usedLabel")]
        used_label: String,
        #[schema(max_length = 120)]
        #[serde(rename = "limitLabel")]
        limit_label: String,
        #[schema(max_length = 500)]
        #[serde(rename = "emptyText")]
        empty_text: String,
        tone: Tone,
    },
    Records {
        #[schema(min_length = 1, max_length = 100, pattern = "^[A-Za-z0-9._-]+$")]
        id: String,
        #[schema(max_length = 200)]
        title: String,
        #[schema(max_length = 500)]
        #[serde(rename = "emptyText")]
        empty_text: String,
        #[schema(min_items = 1, max_items = 4)]
        datasets: Vec<RecordSet>,
    },
}

#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct BarPoint {
    #[schema(max_length = 120)]
    pub label: String,
    #[schema(minimum = -1000000000000000.0, maximum = 1000000000000000.0)]
    pub value: f64,
    pub tone: Tone,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct InlineTable {
    #[schema(min_items = 1, max_items = 32)]
    pub columns: Vec<TableColumn>,
    #[schema(max_items = 500)]
    pub rows: Vec<BTreeMap<String, TableScalar>>,
    #[schema(max_length = 2048)]
    #[ts(optional = nullable)]
    pub caption: Option<String>,
    #[schema(max_length = 2048)]
    #[ts(optional = nullable)]
    pub highlight: Option<String>,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct TableColumn {
    #[schema(min_length = 1, max_length = 2048)]
    pub key: String,
    #[schema(max_length = 2048)]
    pub label: String,
    #[ts(optional = nullable)]
    pub align: Option<TableAlign>,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum TableAlign {
    Left,
    Right,
}
#[derive(Deserialize, ToSchema, TS)]
#[serde(untagged)]
pub enum TableScalar {
    Text(TableText),
    Number(f64),
    Null(TableNull),
}
fn required_nullable<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<f64>, D::Error> {
    Option::<f64>::deserialize(deserializer)
}

#[derive(Deserialize, TS)]
#[serde(transparent)]
pub struct TableText(pub String);
impl utoipa::PartialSchema for TableText {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::schema::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .max_length(Some(super::super::kinds::MAX_STRING_CHARS))
            .into()
    }
}
impl utoipa::ToSchema for TableText {}

/// Explicit null primitive: utoipa's unit schema only supplies a default value.
#[derive(Deserialize, TS)]
#[serde(transparent)]
pub struct TableNull(pub ());
impl utoipa::PartialSchema for TableNull {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::schema::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::Null)
            .into()
    }
}
impl utoipa::ToSchema for TableNull {}
