//! Generic saved chart/table composition. Runtime data joins and financial
//! completeness checks belong to the renderer; this validates configuration.
use super::kinds::{MAX_STRING_CHARS, validate_live_source};
use serde_json::{Map, Value};

type Object = Map<String, Value>;

pub(super) fn validate(map: &Object, errors: &mut Vec<String>) {
    let mut v = Validator { errors };
    v.fields(map, "", &["version", "columns", "gap", "surface", "items"]);
    v.integer(map.get("version"), "version", 1, 1);
    let columns = v.integer(map.get("columns"), "columns", 1, 3);
    v.enumeration(map.get("gap"), "gap", &["compact", "normal", "wide"]);
    v.enumeration(map.get("surface"), "surface", &["plain", "muted"]);
    if let Some(items) = v.array(map.get("items"), "items", 1, 12) {
        for (i, item) in items.iter().enumerate() {
            v.item(item, &format!("items[{i}]"), columns);
        }
    }
}

struct Validator<'a> {
    errors: &'a mut Vec<String>,
}
impl Validator<'_> {
    fn error(&mut self, path: &str, message: &str) {
        self.errors.push(format!("{path}: {message}"));
    }
    fn fields(&mut self, map: &Object, path: &str, allowed: &[&str]) {
        for key in map.keys().filter(|key| !allowed.contains(&key.as_str())) {
            self.error(&join(path, key), "unknown field");
        }
    }
    fn object<'v>(&mut self, value: Option<&'v Value>, path: &str) -> Option<&'v Object> {
        match value.and_then(Value::as_object) {
            Some(map) => Some(map),
            None => {
                self.error(path, "required object");
                None
            }
        }
    }
    fn array<'v>(
        &mut self,
        value: Option<&'v Value>,
        path: &str,
        min: usize,
        max: usize,
    ) -> Option<&'v Vec<Value>> {
        let Some(values) = value.and_then(Value::as_array) else {
            self.error(path, "required array");
            return None;
        };
        if !(min..=max).contains(&values.len()) {
            self.error(path, &format!("must contain {min}..{max} items"));
        }
        Some(values)
    }
    fn text<'v>(
        &mut self,
        value: Option<&'v Value>,
        path: &str,
        nonempty: bool,
    ) -> Option<&'v str> {
        let Some(text) = value.and_then(Value::as_str) else {
            self.error(path, "required string");
            return None;
        };
        if nonempty && text.is_empty() {
            self.error(path, "must not be empty");
        }
        if text.chars().count() > MAX_STRING_CHARS {
            self.error(path, "maximum 2048 Unicode characters");
        }
        Some(text)
    }
    fn integer(&mut self, value: Option<&Value>, path: &str, min: u64, max: u64) -> Option<u64> {
        // JSON 1.0 and 1 denote the same integer in the frontend contract.
        let value = value.and_then(Value::as_f64);
        if let Some(n) = value
            && n.is_finite()
            && n.fract() == 0.0
            && n >= min as f64
            && n <= max as f64
        {
            return Some(n as u64);
        }
        self.error(path, &format!("required integer {min}..{max}"));
        None
    }
    fn enumeration<'v>(
        &mut self,
        value: Option<&'v Value>,
        path: &str,
        allowed: &[&str],
    ) -> Option<&'v str> {
        let text = value.and_then(Value::as_str);
        if !text.is_some_and(|s| allowed.contains(&s)) {
            self.error(path, &format!("required one of {}", allowed.join(" | ")));
        }
        text
    }
    fn scalar(&mut self, value: Option<&Value>, path: &str) {
        match value {
            Some(Value::Null) => {}
            Some(Value::String(_)) => {
                self.text(value, path, false);
            }
            Some(Value::Number(n)) if n.as_f64().is_some_and(f64::is_finite) => {}
            _ => self.error(path, "required string | finite number | null"),
        }
    }
    fn selector(&mut self, value: &Value, path: &str) {
        if let Some(map) = self.object(Some(value), path) {
            self.fields(map, path, &["key", "value"]);
            self.text(map.get("key"), &join(path, "key"), true);
            self.scalar(map.get("value"), &join(path, "value"));
        }
    }
    fn rows<'v>(&mut self, value: Option<&'v Value>, path: &str) -> Option<&'v Vec<Value>> {
        let rows = self.array(value, path, 0, 500)?;
        for (i, row) in rows.iter().enumerate() {
            let path = format!("{path}[{i}]");
            if let Some(row) = self.object(Some(row), &path) {
                if row.len() > 32 {
                    self.error(&path, "maximum 32 keys");
                }
                for (key, value) in row {
                    self.text(Some(&Value::String(key.clone())), &join(&path, key), true);
                    self.scalar(Some(value), &join(&path, key));
                }
            }
        }
        Some(rows)
    }
    fn annotations(&mut self, value: &Value, path: &str) {
        let Some(map) = self.object(Some(value), path) else {
            return;
        };
        self.fields(map, path, &["keys", "rows"]);
        let keys = self.array(map.get("keys"), &join(path, "keys"), 1, 4);
        let mut join_keys = Vec::new();
        if let Some(keys) = keys {
            for (i, key) in keys.iter().enumerate() {
                let p = format!("{path}.keys[{i}]");
                if let Some(key) = self.text(Some(key), &p, true) {
                    if join_keys.contains(&key) {
                        self.error(&p, "duplicate join key");
                    }
                    join_keys.push(key);
                }
            }
        }
        if let Some(rows) = self.rows(map.get("rows"), &join(path, "rows")) {
            let mut tuples: Vec<Vec<&Value>> = Vec::new();
            for (i, row) in rows.iter().enumerate() {
                let Some(row) = row.as_object() else {
                    continue;
                };
                let mut tuple = Vec::new();
                for key in &join_keys {
                    match row.get(*key) {
                        Some(v) if v.is_string() || v.is_number() => tuple.push(v),
                        _ => self.error(
                            &format!("{path}.rows[{i}].{key}"),
                            "join key must be present and non-null",
                        ),
                    }
                }
                if tuple.len() == join_keys.len() && !join_keys.is_empty() {
                    // Scalar numeric equality matches JS (1 and 1.0 are equal),
                    // without conflating numbers and strings or delimiter text.
                    if tuples.iter().any(|old| tuple_equal(old, &tuple)) {
                        self.error(
                            &format!("{path}.rows[{i}]"),
                            "duplicate annotation join tuple",
                        );
                    }
                    tuples.push(tuple);
                }
            }
        }
    }
    fn data(&mut self, value: Option<&Value>, path: &str) {
        let Some(map) = self.object(value, path) else {
            return;
        };
        if map.contains_key("source") {
            self.fields(map, path, &["source", "annotations"]);
            if let Some(source) = self.text(map.get("source"), &join(path, "source"), true)
                && let Err(error) = validate_live_source(source)
            {
                self.error(path, &error);
            }
            if let Some(annotations) = map.get("annotations") {
                self.annotations(annotations, &join(path, "annotations"));
            }
        } else {
            self.fields(map, path, &["rows"]);
            self.rows(map.get("rows"), &join(path, "rows"));
        }
    }
    fn item(&mut self, value: &Value, path: &str, columns: Option<u64>) {
        let Some(map) = self.object(Some(value), path) else {
            return;
        };
        let kind = self.enumeration(map.get("kind"), &join(path, "kind"), &["chart", "table"]);
        let mut fields = vec!["kind", "title", "span", "data", "exclude"];
        match kind {
            Some("chart") => fields.extend([
                "chart",
                "x",
                "y",
                "height",
                "color",
                "unit",
                "ranges",
                "defaultRange",
                "labelSuffixKey",
            ]),
            Some("table") => fields.extend(["columns", "total"]),
            _ => {}
        }
        self.fields(map, path, &fields);
        self.text(map.get("title"), &join(path, "title"), false);
        if let Some(span) = self.integer(map.get("span"), &join(path, "span"), 1, 3)
            && columns.is_some_and(|columns| span > columns)
        {
            self.error(&join(path, "span"), "must not exceed layout columns");
        }
        self.data(map.get("data"), &join(path, "data"));
        if let Some(exclude) = map.get("exclude") {
            self.selector(exclude, &join(path, "exclude"));
        }
        match kind {
            Some("chart") => self.chart(map, path),
            Some("table") => self.table(map, path),
            _ => {}
        }
    }
    fn chart(&mut self, map: &Object, path: &str) {
        let chart = self.enumeration(map.get("chart"), &join(path, "chart"), &["line", "donut"]);
        self.text(map.get("x"), &join(path, "x"), true);
        self.text(map.get("y"), &join(path, "y"), true);
        if map.contains_key("labelSuffixKey") {
            self.text(
                map.get("labelSuffixKey"),
                &join(path, "labelSuffixKey"),
                true,
            );
            if chart != Some("donut") {
                self.error(
                    &join(path, "labelSuffixKey"),
                    "only donut charts accept a label suffix",
                );
            }
        }
        self.integer(map.get("height"), &join(path, "height"), 160, 640);
        if let Some(color) = self.text(map.get("color"), &join(path, "color"), false)
            && !(color.len() == 7
                && color.starts_with('#')
                && color[1..].bytes().all(|b| b.is_ascii_hexdigit()))
        {
            self.error(&join(path, "color"), "must be #RRGGBB");
        }
        if let Some(unit) = map.get("unit")
            && let Some(unit) = self.object(Some(unit), &join(path, "unit"))
        {
            self.fields(unit, &join(path, "unit"), &["key", "equals", "row"]);
            self.text(unit.get("key"), &join(path, "unit.key"), true);
            self.text(unit.get("equals"), &join(path, "unit.equals"), true);
            if let Some(row) = unit.get("row") {
                self.selector(row, &join(path, "unit.row"));
            }
        }
        if map.contains_key("ranges") != map.contains_key("defaultRange") {
            self.error(path, "ranges and defaultRange must be present together");
        }
        let default = if map.contains_key("defaultRange") {
            self.integer(
                map.get("defaultRange"),
                &join(path, "defaultRange"),
                1,
                3660,
            )
        } else {
            None
        };
        if let Some(ranges) = map.get("ranges") {
            if chart != Some("line") {
                self.error(&join(path, "ranges"), "only line charts accept ranges");
            }
            if let Some(ranges) = self.array(Some(ranges), &join(path, "ranges"), 1, 8) {
                let mut previous = 0;
                let mut found_default = false;
                for (i, range) in ranges.iter().enumerate() {
                    if let Some(range) =
                        self.integer(Some(range), &format!("{path}.ranges[{i}]"), 1, 3660)
                    {
                        if range <= previous {
                            self.error(&join(path, "ranges"), "must be unique and ascending");
                        }
                        previous = range;
                        found_default |= Some(range) == default;
                    }
                }
                if !found_default {
                    self.error(&join(path, "defaultRange"), "must occur in ranges");
                }
            }
        }
    }
    fn table(&mut self, map: &Object, path: &str) {
        let mut has_share = false;
        let mut keys = Vec::new();
        if let Some(columns) = self.array(map.get("columns"), &join(path, "columns"), 1, 32) {
            for (i, column) in columns.iter().enumerate() {
                let p = format!("{path}.columns[{i}]");
                let Some(column) = self.object(Some(column), &p) else {
                    continue;
                };
                self.fields(
                    column,
                    &p,
                    &[
                        "key",
                        "label",
                        "format",
                        "digits",
                        "minDigits",
                        "fallbackKey",
                        "suffixKey",
                        "linkKey",
                    ],
                );
                if let Some(key) = self.text(column.get("key"), &join(&p, "key"), true) {
                    if keys.contains(&key) {
                        self.error(&join(&p, "key"), "duplicate column key");
                    }
                    keys.push(key);
                }
                self.text(column.get("label"), &join(&p, "label"), false);
                has_share |= self.enumeration(
                    column.get("format"),
                    &join(&p, "format"),
                    &["text", "number", "percent", "share"],
                ) == Some("share");
                let digits = self.integer(column.get("digits"), &join(&p, "digits"), 0, 8);
                if column.contains_key("minDigits") {
                    let minimum =
                        self.integer(column.get("minDigits"), &join(&p, "minDigits"), 0, 8);
                    if let (Some(minimum), Some(maximum)) = (minimum, digits)
                        && minimum > maximum
                    {
                        self.error(&join(&p, "minDigits"), "must not exceed digits");
                    }
                }
                for key in ["fallbackKey", "suffixKey", "linkKey"] {
                    if column.contains_key(key) {
                        self.text(column.get(key), &join(&p, key), true);
                    }
                }
            }
        }
        if has_share != map.contains_key("total") {
            self.error(
                &join(path, "total"),
                "required if and only if a column uses share",
            );
        }
        if let Some(total) = map.get("total")
            && let Some(total) = self.object(Some(total), &join(path, "total"))
        {
            self.fields(total, &join(path, "total"), &["row", "key"]);
            if let Some(row) = total.get("row") {
                self.selector(row, &join(path, "total.row"));
            } else {
                self.error(&join(path, "total.row"), "required selector");
            }
            self.text(total.get("key"), &join(path, "total.key"), true);
        }
    }
}
fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}
fn tuple_equal(left: &[&Value], right: &[&Value]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| match (a.as_f64(), b.as_f64()) {
                (Some(a), Some(b)) => a == b,
                _ => a == b,
            })
}
