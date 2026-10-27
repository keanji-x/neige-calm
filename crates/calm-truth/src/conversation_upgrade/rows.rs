use super::*;
use sqlx::{Row, SqliteConnection};

pub(super) fn quote(name: &str) -> Result<String, SnapshotError> {
    if name.contains('\0') {
        return Err(unsupported(name, "NUL in SQL identifier"));
    }
    Ok(format!("\"{}\"", name.replace('"', "\"\"")))
}

/// Explicit typeof precedes decoding. CAST TEXT AS BLOB preserves the database
/// encoding (including invalid text); SQLx's direct TEXT bytes can transcode.
pub(super) async fn cells(
    c: &mut SqliteConnection,
    from: &str,
    expressions: &[String],
    order: &str,
) -> Result<Vec<Vec<RawCell>>, SnapshotError> {
    // Bound projection width independently of source column count. Four SQL
    // expressions per cell must not exclude otherwise readable wide tables.
    let mut result: Option<Vec<Vec<RawCell>>> = None;
    for chunk in expressions.chunks(128) {
        let batch = cell_batch(c, from, chunk, order).await?;
        if let Some(rows) = &mut result {
            if rows.len() != batch.len() {
                return Err(unsupported(from, "batch row count mismatch"));
            }
            for (row, values) in rows.iter_mut().zip(batch) {
                row.extend(values);
            }
        } else {
            result = Some(batch);
        }
    }
    result.ok_or_else(|| unsupported(from, "empty raw projection"))
}
async fn cell_batch(
    c: &mut SqliteConnection,
    from: &str,
    expressions: &[String],
    order: &str,
) -> Result<Vec<Vec<RawCell>>, SnapshotError> {
    let projection = expressions
        .iter()
        .map(|v| {
            format!(
                "typeof({v}),CASE WHEN typeof({v}) IN ('text','blob') THEN CAST({v} AS BLOB) END,\
         CASE WHEN typeof({v})='integer' THEN {v} END,CASE WHEN typeof({v})='real' THEN {v} END"
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT {projection} FROM {from} {order}");
    let rows = sqlx::query(&sql)
        .fetch_all(c)
        .await
        .map_err(|e| partial(from, e))?;
    rows.into_iter()
        .map(|r| {
            expressions
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    let i = i * 4;
                    let decode = || -> Result<RawCell, sqlx::Error> {
                        Ok(match r.try_get::<String, _>(i)?.as_str() {
                            "null" => RawCell::Null,
                            "integer" => RawCell::Integer(r.try_get(i + 2)?),
                            "real" => RawCell::RealBits(r.try_get::<f64, _>(i + 3)?.to_bits()),
                            "text" => RawCell::Text(r.try_get(i + 1)?),
                            "blob" => RawCell::Blob(r.try_get(i + 1)?),
                            _ => {
                                return Err(sqlx::Error::Protocol(
                                    "unknown SQLite storage class".into(),
                                ));
                            }
                        })
                    };
                    decode().map_err(|e| partial(from, e))
                })
                .collect()
        })
        .collect()
}

pub(super) async fn relation(
    c: &mut SqliteConnection,
    name: String,
    facts: Vec<Vec<RawCell>>,
    encoding: &str,
    without_rowid: bool,
) -> Result<RawRelation, SnapshotError> {
    let columns = facts
        .iter()
        .map(|f| super::schema::text(&f[1], encoding))
        .collect::<Result<Vec<_>, _>>()?;
    let mut pk = facts
        .iter()
        .enumerate()
        .filter_map(|(i, f)| match f[5] {
            RawCell::Integer(n) if n > 0 => Some((n, i)),
            _ => None,
        })
        .collect::<Vec<_>>();
    pk.sort();
    let alias = if without_rowid {
        None
    } else {
        ["rowid", "_rowid_", "oid"]
            .into_iter()
            .find(|a| !columns.iter().any(|n| n.eq_ignore_ascii_case(a)))
    };
    if alias.is_none() && pk.is_empty() {
        return Err(unsupported(
            &name,
            "no unshadowed rowid or unique primary key",
        ));
    }
    let key_columns = match alias {
        Some(a) => vec![a.to_owned()],
        None => pk.iter().map(|(_, i)| columns[*i].clone()).collect(),
    };
    let keys = key_columns
        .iter()
        .map(|n| quote(n))
        .collect::<Result<Vec<_>, _>>()?;
    let stored = facts
        .iter()
        .enumerate()
        .filter(|(_, f)| f[6] != RawCell::Integer(2))
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let mut expr = keys.clone();
    for i in &stored {
        expr.push(quote(&columns[*i])?);
    }
    let from = format!("main.{}", quote(&name)?);
    let order = format!("ORDER BY {}", keys.join(","));
    let source_count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {from}"))
        .fetch_one(&mut *c)
        .await
        .map_err(|e| partial(&name, e))?;
    let raw = cells(c, &from, &expr, &order).await?;
    let mut rows: Vec<RawRow> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for values in raw {
        let key = values[..keys.len()].to_vec();
        if key.contains(&RawCell::Null) || !seen.insert(key.clone()) {
            return Err(unsupported(&name, "NULL or duplicate stable key"));
        }
        let mut cells = vec![RawCell::DerivedNotStored; columns.len()];
        for (offset, i) in stored.iter().enumerate() {
            cells[*i] = values[keys.len() + offset].clone();
        }
        rows.push(RawRow { key, cells });
    }
    if rows.len() as i64 != source_count {
        return Err(unsupported(&name, "row count mismatch"));
    }
    Ok(RawRelation {
        name,
        columns,
        column_facts: facts,
        key_columns,
        rows,
        source_count,
    })
}
