//! Test-only direct SQL oracle: expected sources never depend on inventory or visitor.
use super::*;
use sha2::{Digest, Sha256};
use sqlx::{Connection, Row, SqliteConnection};

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
async fn tuples(
    c: &mut SqliteConnection,
    from: &str,
    columns: &[String],
    order: &str,
) -> Vec<Vec<RawCell>> {
    let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {from}"))
        .fetch_one(&mut *c)
        .await
        .unwrap();
    let mut expected = vec![Vec::new(); count as usize];
    // Separate direct queries keep the oracle independent of B1 batching and
    // SQLx TEXT conversion. Hex(CAST AS BLOB) is the original database encoding.
    for column in columns {
        let sql = format!(
            "SELECT typeof({column}),hex(CAST({column} AS BLOB)),{column} FROM {from} {order}"
        );
        let rows = sqlx::query(&sql).fetch_all(&mut *c).await.unwrap();
        assert_eq!(rows.len(), expected.len());
        for (target, row) in expected.iter_mut().zip(rows) {
            let class: String = row.get(0);
            target.push(match class.as_str() {
                "null" => RawCell::Null,
                "integer" => RawCell::Integer(row.get(2)),
                "real" => RawCell::RealBits(row.get::<f64, _>(2).to_bits()),
                "text" => RawCell::Text(hex::decode(row.get::<String, _>(1)).unwrap()),
                "blob" => RawCell::Blob(hex::decode(row.get::<String, _>(1)).unwrap()),
                _ => panic!("unknown SQL storage class"),
            });
        }
    }
    expected
}
fn identifier(cell: &RawCell, encoding: &str) -> String {
    let RawCell::Text(b) = cell else {
        panic!("metadata name")
    };
    match encoding {
        "UTF-8" => String::from_utf8(b.clone()).unwrap(),
        _ => String::from_utf16(
            &b.as_chunks::<2>()
                .0
                .iter()
                .map(|b| {
                    if encoding == "UTF-16le" {
                        u16::from_le_bytes([b[0], b[1]])
                    } else {
                        u16::from_be_bytes([b[0], b[1]])
                    }
                })
                .collect::<Vec<_>>(),
        )
        .unwrap(),
    }
}
pub(super) async fn expected(f: &raw_tests::Fixture) -> Vec<SourceFact> {
    let bytes = std::fs::read(&f.path).unwrap();
    let sha = Sha256::digest(&bytes).into();
    let mut c = f.oracle().await;
    let encoding: String = sqlx::query_scalar("PRAGMA encoding")
        .fetch_one(&mut c)
        .await
        .unwrap();
    let mut result = Vec::new();
    let mut push = |location, kind, digest| {
        result.push(SourceFact {
            source: SourceRef {
                source_sha256: sha,
                source_len: bytes.len() as u64,
                namespace: "main",
                location,
                evidence_sha256: digest,
            },
            kind,
            blocked: BlockedReason::Unclassified,
        })
    };
    push(
        SourceLocation::Encoding,
        SourceKind::Encoding,
        evidence_digest("encoding", &encoding).unwrap(),
    );
    let names = |s: &[&str]| s.iter().map(|s| quote(s)).collect::<Vec<_>>();
    let schema = tuples(
        &mut c,
        "sqlite_schema",
        &names(&["type", "name", "tbl_name", "rootpage", "sql"]),
        "ORDER BY rowid",
    )
    .await;
    for (ordinal, tuple) in schema.iter().enumerate() {
        push(
            SourceLocation::Schema { ordinal },
            SourceKind::Schema,
            evidence_digest("schema", tuple).unwrap(),
        );
    }
    let tables = tuples(
        &mut c,
        "pragma_table_list WHERE schema='main'",
        &names(&["schema", "name", "type", "ncol", "wr", "strict"]),
        "ORDER BY name",
    )
    .await;
    for (ordinal, tuple) in tables.iter().enumerate() {
        push(
            SourceLocation::TableList { ordinal },
            SourceKind::TableList,
            evidence_digest("table_list", tuple).unwrap(),
        );
    }
    for table in tables {
        if identifier(&table[2], &encoding) != "table" {
            continue;
        }
        let name = identifier(&table[1], &encoding);
        let from = format!("pragma_table_xinfo('{}')", name.replace('\'', "''"));
        let metadata = tuples(
            &mut c,
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
        .await;
        let columns = metadata
            .iter()
            .map(|m| identifier(&m[1], &encoding))
            .collect::<Vec<_>>();
        let mut pk = metadata
            .iter()
            .enumerate()
            .filter_map(|(i, m)| match m[5] {
                RawCell::Integer(n) if n > 0 => Some((n, i)),
                _ => None,
            })
            .collect::<Vec<_>>();
        pk.sort();
        let key_columns = if table[4] == RawCell::Integer(1) {
            pk.iter().map(|(_, i)| columns[*i].clone()).collect()
        } else {
            vec!["rowid".to_owned()]
        }; // fixture has no rowid-shadowing tables
        let table_from = quote(&name);
        let order = format!(
            "ORDER BY {}",
            key_columns
                .iter()
                .map(|s| quote(s))
                .collect::<Vec<_>>()
                .join(",")
        );
        let keys = tuples(
            &mut c,
            &table_from,
            &key_columns.iter().map(|s| quote(s)).collect::<Vec<_>>(),
            &order,
        )
        .await;
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table_from}"))
            .fetch_one(&mut c)
            .await
            .unwrap();
        push(
            SourceLocation::Relation { name: name.clone() },
            SourceKind::Relation,
            evidence_digest("relation", &(&name, &columns, &key_columns, count)).unwrap(),
        );
        for (ordinal, tuple) in metadata.iter().enumerate() {
            push(
                SourceLocation::ColumnMetadata {
                    relation: name.clone(),
                    ordinal,
                },
                SourceKind::ColumnMetadata,
                evidence_digest("xinfo", tuple).unwrap(),
            );
        }
        let mut cells = vec![Vec::new(); count as usize];
        for (i, column) in columns.iter().enumerate() {
            let values = if metadata[i][6] == RawCell::Integer(2) {
                vec![vec![RawCell::DerivedNotStored]; count as usize]
            } else {
                tuples(&mut c, &table_from, &[quote(column)], &order).await
            };
            for (row, value) in cells.iter_mut().zip(values) {
                row.push(value[0].clone());
            }
        }
        for (key, cells) in keys.into_iter().zip(cells) {
            let raw = RawRow { key, cells };
            let row = RowSource {
                relation: name.clone(),
                raw_key: raw.key.clone(),
                declared_key: pk
                    .iter()
                    .map(|(_, i)| (columns[*i].clone(), raw.cells[*i].clone()))
                    .collect(),
                row_sha256: evidence_digest("row", &raw).unwrap(),
            };
            push(
                SourceLocation::Row(row.clone()),
                SourceKind::Row,
                row.row_sha256,
            );
            for (column_index, (column_name, cell)) in columns.iter().zip(&raw.cells).enumerate() {
                let hash = evidence_digest("cell", cell).unwrap();
                push(
                    SourceLocation::Cell {
                        row: row.clone(),
                        column_index,
                        column_name: column_name.clone(),
                        cell_sha256: hash,
                        json: None,
                    },
                    match cell {
                        RawCell::Null => SourceKind::SqlNull,
                        RawCell::DerivedNotStored => SourceKind::DerivedNotStored,
                        _ => SourceKind::Cell,
                    },
                    hash,
                );
            }
        }
    }
    c.close().await.unwrap();
    result
}
