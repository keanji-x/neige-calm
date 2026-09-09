//! Agent-facing schema for the generic saved Report layout (#1595).
//! Cross-field and join constraints described below are enforced by `layout`.
use serde_json::{Value, json};

pub fn schema() -> Value {
    let text = json!({"type":"string","maxLength":2048});
    let key = json!({"type":"string","minLength":1,"maxLength":2048});
    let scalar = json!({"type":["string","number","null"],"maxLength":2048});
    let row = json!({"type":"object","maxProperties":32,"propertyNames":key,"additionalProperties":scalar});
    let rows = json!({"type":"array","maxItems":500,"items":row});
    let selector = json!({"type":"object","additionalProperties":false,"required":["key","value"],"properties":{"key":key,"value":scalar}});
    let annotations = json!({
        "type":"object","additionalProperties":false,"required":["keys","rows"],
        "properties":{"keys":{"type":"array","minItems":1,"maxItems":4,"uniqueItems":true,"items":key},"rows":rows},
        "description":"Each annotation row must contain all join keys as non-null scalars; tuples must be unique (enforced server-side). Runtime joins reject duplicate producer tuples and annotations that overwrite any producer field except identical join keys."
    });
    let data = json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["rows"],"properties":{"rows":rows}},
        {"type":"object","additionalProperties":false,"required":["source"],"properties":{
            "source":{"type":"string","maxLength":2048,"pattern":"^neige://plugin/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+(?![\\s\\S])","description":"Plugin overlay for the current Track only; no Track ID or arbitrary URL."},
            "annotations":annotations
        }}
    ]});
    let unit = json!({"type":"object","additionalProperties":false,"required":["key","equals"],"properties":{"key":key,"equals":key,"row":selector},
        "description":"Without row, observations must match equals: mismatches create line gaps and prevent donut normalization. With row, exactly one row before exclude must match, with unit key equal to equals, or the whole chart is unavailable. The template declares the currency; no implicit conversion or relabeling."
    });
    let mut column = json!({"type":"object","additionalProperties":false,"required":["key","label","format","digits"],"properties":{
        "key":key,"label":text,"format":{"enum":["text","number","percent","share"]},"digits":{"type":"integer","minimum":0,"maximum":8},"minDigits":{"type":"integer","minimum":0,"maximum":8},"fallbackKey":key,"suffixKey":key,"linkKey":key
    },"description":"Column keys must be unique (server-enforced). Numeric formats use digits as maximum decimal places and optional minDigits as minimum; minDigits must not exceed digits. Without minDigits, use exactly digits places, preserving fixed-decimal templates. Missing values are a dash. Percent uses supplied percentage without multiplying. Share requires a complete nonnegative column whose selected values sum to the positive selected total within independent cent rounding; no fallback values in share arithmetic. linkKey contains a Track ID opened through native navigation, never an arbitrary URL. suffixKey is a display label only, never conversion."});
    column["allOf"] = Value::Array(
        (0..=8)
            .map(|digits| {
                json!({
                    "if":{"required":["digits"],"properties":{"digits":{"const":digits}}},
                    "then":{"properties":{"minDigits":{"maximum":digits}}}
                })
            })
            .collect(),
    );
    let total = json!({"type":"object","additionalProperties":false,"required":["row","key"],"properties":{"row":selector,"key":key},"description":"Exactly one source row before exclude must match. Required iff any column uses share (server-enforced)."});
    let mut chart = json!({"type":"object","additionalProperties":false,"required":["kind","title","span","data","chart","x","y","height","color"],"properties":{
        "kind":{"const":"chart"},"title":text,"span":{"type":"integer","minimum":1,"maximum":3},"data":data,"exclude":selector,
        "chart":{"enum":["line","donut"]},"x":key,"y":key,"labelSuffixKey":key,"height":{"type":"integer","minimum":160,"maximum":640},"color":{"type":"string","pattern":"^#[A-Fa-f0-9]{6}$","minLength":7,"maxLength":7},"unit":unit,
        "ranges":{"type":"array","minItems":1,"maxItems":8,"uniqueItems":true,"items":{"type":"integer","minimum":1,"maximum":3660}},"defaultRange":{"type":"integer","minimum":1,"maximum":3660}
    }});
    chart["allOf"] = json!([
        {"if":{"required":["ranges"]},"then":{"required":["defaultRange"],"properties":{"chart":{"const":"line"}}}},
        {"if":{"required":["defaultRange"]},"then":{"required":["ranges"]}},
        {"if":{"required":["labelSuffixKey"]},"then":{"properties":{"chart":{"const":"donut"}}}}
    ]);
    chart["description"] = json!(
        "Ranges are ascending trailing days from latest x; defaultRange must occur in ranges (server-enforced). Line: x is epoch milliseconds or a parseable date, stable timestamp order including duplicates; invalid x is an error, missing/invalid y a gap; at least two usable observations. Donut: every selected y must be finite and nonnegative, every x a non-null scalar, or unavailable; empty/all-zero stays empty. Optional labelSuffixKey is donut-only: labels become x · suffix, with a required non-null scalar suffix in every selected row; missing/invalid suffix makes the chart unavailable. Exclude summary rows explicitly. Producer caption is preserved separately from title."
    );
    let table = json!({"type":"object","additionalProperties":false,"required":["kind","title","span","data","columns"],"properties":{
        "kind":{"const":"table"},"title":text,"span":{"type":"integer","minimum":1,"maximum":3},"data":data,"exclude":selector,
        "columns":{"type":"array","minItems":1,"maxItems":32,"items":column},"total":total
    }});
    json!({"type":"object","additionalProperties":false,"required":["version","columns","gap","surface","items"],"properties":{
        "version":{"const":1},"columns":{"type":"integer","minimum":1,"maximum":3},"gap":{"enum":["compact","normal","wide"]},"surface":{"enum":["plain","muted"]},
        "items":{"type":"array","minItems":1,"maxItems":12,"items":{"oneOf":[chart,table]}}
    },"description":"Saved generic composition of native chart/table primitives. Each item span must not exceed root columns (server-enforced). All strings, including row keys, are limited to 2048 Unicode characters. JSON numbers must be finite. Maximum canonical JSON size 256KB per block. Runtime data errors stay visible; unavailable observations never become invented values."})
}
