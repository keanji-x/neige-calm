use super::schema_checks::accepts;
use super::*;

fn value(config: Value, mode: Value) -> Value {
    json!({"key":"build.v1","title":"Build","mode":mode,
        "checks":[{"id":"tests","role":"goal","config":config}],"constraints":[],
        "budget":{"turns":10,"tokens":1000,"wall_secs":60},"compact":"none"})
}
#[test]
fn objective_schema_parity_for_every_check_kind_and_closed_fields() {
    let table = kinds_table();
    let schema = &table["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["kind"] == "objective")
        .unwrap()["schema"];
    assert_eq!(schema, &report_blocks::objectives::schema());
    let configs = [
        json!({"kind":"command","params":{"command":"true"}}),
        json!({"kind":"metric","params":{"command":"bench","direction":"lower","tolerance":0.1}}),
        json!({"kind":"metric","params":{"command":"bench","direction":"higher"}}),
        json!({"kind":"evaluator","params":{"rubric":[{"id":"r","criterion":"ok"}],"samples":1,"aggregate":"median"}}),
        json!({"kind":"fact","params":{"fact":"pr_merged","subject":"pr"}}),
    ];
    for config in configs {
        let valid = value(config, json!("converge"));
        assert!(accepts(schema, &valid, schema), "{valid}");
        assert!(
            report_blocks::validate_payload("objective", &valid).is_ok(),
            "{valid}"
        );
        // Every object is closed, and all serialized fields other than optional tolerance are required.
        fn sweep(value: &Value, schema: &Value, root: &Value, base: &Value, path: &str) {
            match value {
                Value::Object(fields) => {
                    let mut unknown = base.clone();
                    unknown
                        .pointer_mut(path)
                        .unwrap()
                        .as_object_mut()
                        .unwrap()
                        .insert("unknown".into(), json!(true));
                    assert!(!accepts(schema, &unknown, root), "{unknown}");
                    assert!(report_blocks::validate_payload("objective", &unknown).is_err());
                    for (key, child) in fields {
                        if key != "tolerance" {
                            let mut missing = base.clone();
                            missing
                                .pointer_mut(path)
                                .unwrap()
                                .as_object_mut()
                                .unwrap()
                                .remove(key);
                            assert!(!accepts(schema, &missing, root), "{missing}");
                            assert!(
                                report_blocks::validate_payload("objective", &missing).is_err()
                            );
                        }
                        sweep(child, schema, root, base, &format!("{path}/{key}"));
                    }
                }
                Value::Array(items) => {
                    for (i, child) in items.iter().enumerate() {
                        sweep(child, schema, root, base, &format!("{path}/{i}"));
                    }
                }
                _ => {}
            }
        }
        sweep(&valid, schema, schema, &valid, "");
    }
}
