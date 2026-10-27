use super::*;
use sqlx::SqliteConnection;

pub(super) fn text(value: &RawCell, encoding: &str) -> Result<String, SnapshotError> {
    let RawCell::Text(bytes) = value else {
        return Err(unsupported("identifier", "non-text name"));
    };
    let decoded = match encoding {
        "UTF-8" => String::from_utf8(bytes.clone()).ok(),
        "UTF-16le" | "UTF-16be" if bytes.len() % 2 == 0 => {
            let words = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| {
                    if encoding == "UTF-16le" {
                        u16::from_le_bytes([b[0], b[1]])
                    } else {
                        u16::from_be_bytes([b[0], b[1]])
                    }
                })
                .collect::<Vec<_>>();
            String::from_utf16(&words).ok()
        }
        _ => None,
    };
    decoded.ok_or_else(|| {
        unsupported(
            &format!("identifier bytes {}", hex::encode(bytes)),
            "cannot losslessly decode SQL identifier",
        )
    })
}
fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| format!("\"{s}\"")).collect()
}

pub(super) async fn inventory(c: &mut SqliteConnection) -> Result<RawInventory, SnapshotError> {
    let encoding: String = sqlx::query_scalar("PRAGMA encoding")
        .fetch_one(&mut *c)
        .await
        .map_err(|e| partial("encoding", e))?;
    if !["UTF-8", "UTF-16le", "UTF-16be"].contains(&encoding.as_str()) {
        return Err(unsupported("encoding", &encoding));
    }
    let schema = rows::cells(
        c,
        "main.sqlite_schema",
        &names(&["type", "name", "tbl_name", "rootpage", "sql"]),
        "ORDER BY rowid",
    )
    .await?;
    // Request only type=table facts for xinfo: broken views must remain raw DDL
    // without preparing their SELECT or executing any module/view/trigger.
    let mut objects = Vec::new();
    for object in &schema {
        if text(&object[0], &encoding)? == "table" {
            objects.push(text(&object[1], &encoding)?);
        }
    }
    // Ordinary b-trees have a positive root page. Reject rootless table
    // modules before table_list/xinfo can cause a virtual module connection.
    for object in &schema {
        if text(&object[0], &encoding)? == "table" && object[3] == RawCell::Integer(0) {
            return Err(unsupported(
                &text(&object[1], &encoding)?,
                "virtual module read closure and stored extent unproven",
            ));
        }
    }
    // The private, empty temp namespace prevents source tables with PRAGMA
    // function names from shadowing SQLite's metadata functions. No temp writes.
    let table_list = rows::cells(
        c,
        "temp.pragma_table_list() WHERE schema='main'",
        &names(&["schema", "name", "type", "ncol", "wr", "strict"]),
        "ORDER BY name",
    )
    .await?;
    let mut relations = Vec::new();
    for entry in &table_list {
        let kind = text(&entry[2], &encoding)?;
        if kind == "view" {
            continue;
        }
        let name = text(&entry[1], &encoding)?;
        if kind != "table" {
            return Err(unsupported(&name, "unsupported stored relation kind"));
        }
        let quoted_literal = format!("'{}'", name.replace('\'', "''"));
        let from = format!("temp.pragma_table_xinfo({quoted_literal}, 'main')");
        let facts = rows::cells(
            c,
            &from,
            &names(&[
                "cid",
                "name",
                "type",
                "notnull",
                "dflt_value",
                "pk",
                "hidden",
            ]),
            "ORDER BY cid",
        )
        .await?;
        if facts.len() as i64
            != match entry[3] {
                RawCell::Integer(n) => n,
                _ => -1,
            }
            || facts
                .iter()
                .any(|f| !matches!(f[6], RawCell::Integer(0 | 2 | 3)))
        {
            return Err(unsupported(&name, "column metadata coverage mismatch"));
        }
        relations.push(
            rows::relation(c, name, facts, &encoding, entry[4] == RawCell::Integer(1)).await?,
        );
    }
    for name in objects {
        if !relations.iter().any(|r| r.name == name) {
            return Err(unsupported(&name, "schema/table_list coverage mismatch"));
        }
    }
    Ok(RawInventory {
        encoding,
        schema,
        table_list,
        relations,
    })
}
