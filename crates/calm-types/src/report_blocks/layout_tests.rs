use super::kinds::validate_payload;
use serde_json::{Value, json};

fn chart() -> Value {
    json!({"version":1,"columns":2,"gap":"normal","surface":"plain","items":[{
        "kind":"chart","title":"History","span":2,
        "data":{"source":"neige://plugin/market/history","annotations":{"keys":["asset","venue"],"rows":[{"asset":"AAA","venue":"US","track":"research"}]}},
        "chart":"line","x":"at","y":"value","height":240,"color":"#A1b2C3","ranges":[30,90],"defaultRange":90,
        "unit":{"key":"currency","equals":"CNY","row":{"key":"asset","value":"Total"}},"exclude":{"key":"asset","value":"Total"}
    }]})
}
fn table() -> Value {
    json!({"version":1,"columns":1,"gap":"compact","surface":"muted","items":[{
        "kind":"table","title":"Holdings","span":1,"data":{"rows":[{"__proto__":"safe","name":"甲","value":0},{}]},
        "columns":[{"key":"value","label":"Weight","format":"share","digits":2,"fallbackKey":"other","suffixKey":"currency","linkKey":"track"}],
        "total":{"row":{"key":"name","value":"Total"},"key":"value"}
    }]})
}
#[test]
fn layout_validates_complete_chart_and_table_configuration() {
    for mut payload in [chart(), table()] {
        assert_eq!(validate_payload("layout", &payload), Ok(()));
        payload["version"] = json!(1.0);
        assert_eq!(
            validate_payload("layout", &payload),
            Ok(()),
            "JSON integers include 1.0"
        );
    }
}

#[test]
fn layout_optional_min_digits_roundtrips_without_rewriting_legacy_columns() {
    let legacy = table();
    assert_eq!(validate_payload("layout", &legacy), Ok(()));
    let mut flexible = legacy.clone();
    flexible["items"][0]["columns"][0]["digits"] = json!(8);
    flexible["items"][0]["columns"][0]["minDigits"] = json!(2);
    let fence = super::render_data_block("layout", &flexible)
        .expect("optional precision is saved configuration");
    assert_eq!(super::parse_fence(&fence).unwrap().payload, flexible);
    for digits in [0, 8] {
        let mut boundary = flexible.clone();
        boundary["items"][0]["columns"][0]["digits"] = json!(digits);
        boundary["items"][0]["columns"][0]["minDigits"] = json!(digits);
        assert_eq!(validate_payload("layout", &boundary), Ok(()));
    }
    let old_fence = super::render_data_block("layout", &legacy).unwrap();
    assert_eq!(super::parse_fence(&old_fence).unwrap().payload, legacy);
    for minimum in [
        json!(-1),
        json!(9),
        json!(1.5),
        json!(null),
        json!("2"),
        json!(false),
    ] {
        let mut invalid = flexible.clone();
        invalid["items"][0]["columns"][0]["minDigits"] = minimum;
        assert!(
            validate_payload("layout", &invalid)
                .unwrap_err()
                .contains("minDigits")
        );
    }
    flexible["items"][0]["columns"][0]["digits"] = json!(1);
    assert!(
        validate_payload("layout", &flexible)
            .unwrap_err()
            .contains("minDigits")
    );
}
#[test]
fn layout_rejects_unknown_fields_and_invalid_boundaries() {
    let cases = [
        ("/unknown", json!(0)),
        ("/version", json!(2)),
        ("/columns", json!(0)),
        ("/gap", json!("huge")),
        ("/surface", json!(null)),
        ("/items", json!([])),
        ("/items", json!(vec![chart()["items"][0].clone(); 13])),
        ("/items/0/span", json!(3)),
        ("/items/0/kind", json!("portfolio")),
        ("/items/0/title", json!("字".repeat(2049))),
        ("/items/0/data/source", json!("https://example.com")),
        ("/items/0/data/rows", json!([])),
        ("/items/0/data/annotations", json!(null)),
        ("/items/0/data/annotations/keys", json!(["asset", "asset"])),
        ("/items/0/data/annotations/rows", json!([{"asset":"AAA"}])),
        (
            "/items/0/data/annotations/rows",
            json!([{"asset":"AAA","venue":null}]),
        ),
        (
            "/items/0/data/annotations/rows",
            json!([{"asset":"AAA","venue":1},{"asset":"AAA","venue":1.0}]),
        ),
        (
            "/items/0/data/annotations/rows",
            json!(vec![json!({"asset":"AAA","venue":"US"}); 501]),
        ),
        ("/items/0/chart", json!("donut")),
        ("/items/0/x", json!("")),
        ("/items/0/y", json!(null)),
        ("/items/0/height", json!(159)),
        ("/items/0/height", json!(640.5)),
        ("/items/0/color", json!("red")),
        ("/items/0/ranges", json!([90, 30])),
        ("/items/0/ranges", json!([30, 30])),
        ("/items/0/ranges", json!([])),
        ("/items/0/ranges", json!([30, 3661])),
        ("/items/0/defaultRange", json!(31)),
        ("/items/0/unit/equals", json!("")),
        ("/items/0/unit/row/value", json!(true)),
        ("/items/0/exclude/extra", json!(0)),
    ];
    for (path, value) in cases {
        let mut payload = chart();
        let (parent, key) = path.rsplit_once('/').unwrap();
        payload
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.to_string(), value);
        assert!(
            validate_payload("layout", &payload).is_err(),
            "accepted {path}: {payload}"
        );
    }
    for field in ["ranges", "defaultRange"] {
        let mut payload = chart();
        payload["items"][0].as_object_mut().unwrap().remove(field);
        assert!(
            validate_payload("layout", &payload).is_err(),
            "missing {field}"
        );
    }
}
#[test]
fn layout_rejects_invalid_table_and_inline_rows() {
    let cases = [
        (
            "/items/0/data/annotations",
            json!({"keys":["name"],"rows":[]}),
        ),
        ("/items/0/data/rows", json!([{"name":true}])),
        ("/items/0/data/rows", json!([{ "":1 }])),
        (
            "/items/0/data/rows",
            json!([(0..33)
                .map(|i| (i.to_string(), json!(i)))
                .collect::<serde_json::Map<_, _>>()]),
        ),
        ("/items/0/columns", json!([])),
        (
            "/items/0/columns",
            json!(vec![table()["items"][0]["columns"][0].clone(); 2]),
        ),
        ("/items/0/columns/0/digits", json!(9)),
        ("/items/0/columns/0/format", json!("text")),
        ("/items/0/columns/0/linkKey", json!(null)),
        ("/items/0/total", json!(null)),
        ("/items/0/total/row/extra", json!(0)),
    ];
    for (path, value) in cases {
        let mut payload = table();
        let (parent, key) = path.rsplit_once('/').unwrap();
        payload
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.to_string(), value);
        assert!(
            validate_payload("layout", &payload).is_err(),
            "accepted {path}: {payload}"
        );
    }
    let mut payload = table();
    payload["items"][0].as_object_mut().unwrap().remove("total");
    assert!(validate_payload("layout", &payload).is_err());
}
#[test]
fn layout_join_tuples_are_typed_and_delimiter_safe() {
    let mut payload = chart();
    payload["items"][0]["data"]["annotations"]["rows"] = json!([
        {"asset":1,"venue":"A|B"},{"asset":"1","venue":"A|B"},{"asset":"1|A","venue":"B"}
    ]);
    assert_eq!(validate_payload("layout", &payload), Ok(()));
}

#[test]
fn layout_donut_label_suffix_is_a_saved_nonempty_key_only_for_donuts() {
    let mut payload = chart();
    let item = payload["items"][0].as_object_mut().unwrap();
    item.insert("chart".into(), json!("donut"));
    item.remove("ranges");
    item.remove("defaultRange");
    item.insert("labelSuffixKey".into(), json!("venue"));
    let fence = super::render_data_block("layout", &payload)
        .expect("donut venue suffix is saved configuration");
    assert_eq!(super::parse_fence(&fence).unwrap().payload, payload);
    for suffix in [json!(""), json!(null), json!(42), json!("字".repeat(2049))] {
        let mut invalid = payload.clone();
        invalid["items"][0]["labelSuffixKey"] = suffix;
        assert!(validate_payload("layout", &invalid).is_err());
    }
    payload["items"][0]["chart"] = json!("line");
    assert!(
        validate_payload("layout", &payload)
            .unwrap_err()
            .contains("labelSuffixKey")
    );
    let mut table = table();
    table["items"][0]["labelSuffixKey"] = json!("venue");
    assert!(validate_payload("layout", &table).is_err());
}
