//! Independent SQL bytes -> ordered serde tree -> source-address oracle.
//! No call to source_facts, strict_json, inventory, or classifier builds expected.
use super::{raw_tests::Fixture, *};
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, SeqAccess, Visitor},
};
use sqlx::{Connection, Row};
use std::fmt;

struct Tree(JsonKind, Vec<(Option<String>, Tree)>);
impl<'de> Deserialize<'de> for Tree {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Read;
        impl<'de> Visitor<'de> for Read {
            type Value = Tree;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an ordered JSON tree")
            }
            fn visit_unit<E>(self) -> Result<Tree, E> {
                Ok(Tree(JsonKind::Null, vec![]))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Tree, E> {
                Ok(Tree(JsonKind::Bool, vec![]))
            }
            fn visit_i64<E>(self, _: i64) -> Result<Tree, E> {
                Ok(Tree(JsonKind::Number, vec![]))
            }
            fn visit_u64<E>(self, _: u64) -> Result<Tree, E> {
                Ok(Tree(JsonKind::Number, vec![]))
            }
            fn visit_f64<E>(self, _: f64) -> Result<Tree, E> {
                Ok(Tree(JsonKind::Number, vec![]))
            }
            fn visit_str<E>(self, _: &str) -> Result<Tree, E> {
                Ok(Tree(JsonKind::String, vec![]))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Tree, A::Error> {
                let mut children = vec![];
                while let Some(child) = a.next_element::<Tree>()? {
                    children.push((None, child));
                }
                Ok(Tree(JsonKind::Array, children))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Tree, A::Error> {
                let mut children = vec![];
                while let Some(pair) = a.next_entry::<String, Tree>()? {
                    children.push((Some(pair.0), pair.1));
                }
                Ok(Tree(JsonKind::Object, children))
            }
        }
        d.deserialize_any(Read)
    }
}
fn flatten(t: &Tree, path: Vec<JsonStep>, out: &mut Vec<(JsonAddress, JsonKind)>) {
    out.push((JsonAddress(path.clone()), t.0));
    for (ordinal, (name, child)) in t.1.iter().enumerate() {
        let step = match name {
            None => JsonStep::Index(ordinal),
            Some(name) => JsonStep::Member {
                name: name.clone(),
                ordinal,
                occurrence: t.1[..ordinal]
                    .iter()
                    .filter(|(prior, _)| prior.as_ref() == Some(name))
                    .count(),
            },
        };
        let mut child_path = path.clone();
        child_path.push(step);
        flatten(child, child_path, out);
    }
}
pub(super) async fn assert_all(fixture: &Fixture, facts: &CompleteFactAudit<'_>) {
    let raw = super::fact_oracle::expected(fixture).await;
    let mut c = fixture.oracle().await;
    // This oracle's SQL addressing is deliberately bounded to these fixtures;
    // production inventory retains its independent UTF-16 / WITHOUT ROWID path.
    let encoding: String = sqlx::query_scalar("PRAGMA encoding")
        .fetch_one(&mut c)
        .await
        .unwrap();
    assert_eq!(encoding, "UTF-8");
    let wr: i64 = sqlx::query_scalar(
        "SELECT wr FROM pragma_table_list WHERE schema='main' AND name='worker_sessions'",
    )
    .fetch_one(&mut c)
    .await
    .unwrap();
    assert_eq!(wr, 0);
    let actual_raw: Vec<_> = facts
        .sources()
        .nodes()
        .iter()
        .filter(|n| !matches!(n.kind, SourceKind::Json(_)))
        .map(|n| (&n.source, &n.kind))
        .collect();
    assert_eq!(
        actual_raw,
        raw.iter().map(|n| (&n.source, &n.kind)).collect::<Vec<_>>()
    );
    let columns =
        sqlx::query("SELECT cid,name FROM pragma_table_xinfo('worker_sessions') ORDER BY cid")
            .fetch_all(&mut c)
            .await
            .unwrap();
    let mut expected = vec![];
    let mut duplicates = vec![];
    assert!(columns.iter().all(|col| {
        !["rowid", "_rowid_", "oid"]
            .iter()
            .any(|alias| col.get::<String, _>(1).eq_ignore_ascii_case(alias))
    }));
    for col in columns {
        let name: String = col.get(1);
        if !matches!(
            name.as_str(),
            "handle_state_json" | "provider_execution_json"
        ) {
            continue;
        }
        let sql = format!(
            "SELECT rowid,typeof({name}),hex(CAST({name} AS BLOB)) FROM worker_sessions ORDER BY rowid"
        );
        for row in sqlx::query(&sql).fetch_all(&mut c).await.unwrap() {
            let key = vec![RawCell::Integer(row.get(0))];
            let class: String = row.get(1);
            if class != "text" {
                continue;
            }
            let bytes = hex::decode(row.get::<String, _>(2)).unwrap();
            let cell = RawCell::Text(bytes.clone());
            let source = raw.iter().find(|n| matches!(&n.source.location,
                SourceLocation::Cell { row, column_name, json: None, .. } if row.raw_key == key && column_name == &name)).unwrap();
            assert_eq!(
                source.source.evidence_sha256,
                evidence_digest("cell", &cell).unwrap()
            );
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let Ok(tree) = serde_json::from_str::<Tree>(text) else {
                continue;
            };
            let mut nodes = vec![];
            flatten(&tree, vec![], &mut nodes);
            let bind = |a: JsonAddress| {
                let mut s = source.source.clone();
                if let SourceLocation::Cell { json, .. } = &mut s.location {
                    *json = Some(a);
                }
                s
            };
            for (address, kind) in &nodes {
                if let Some(JsonStep::Member {
                    name, occurrence, ..
                }) = address.0.last()
                    && *occurrence > 0
                {
                    let first = nodes.iter().find(|(a, _)| a.0.len() == address.0.len()
                        && a.0[..a.0.len()-1] == address.0[..address.0.len()-1]
                        && matches!(a.0.last(), Some(JsonStep::Member { name: n, occurrence: 0, .. }) if n == name)).unwrap();
                    duplicates.push(DuplicateMember {
                        first: bind(first.0.clone()),
                        repeated: bind(address.clone()),
                    });
                }
                expected.push((bind(address.clone()), *kind));
            }
        }
    }
    c.close().await.unwrap();
    let actual: Vec<_> = facts
        .sources()
        .nodes()
        .iter()
        .filter_map(|n| {
            if let SourceKind::Json(kind) = n.kind {
                Some((n.source.clone(), kind))
            } else {
                None
            }
        })
        .collect();
    // Order across columns is not a contract; full addresses retain each
    // object's original ordinal, occurrence, and complete ancestor chain.
    assert_eq!(actual.len(), expected.len());
    for n in &expected {
        assert!(actual.contains(n), "{n:?}");
    }
    assert_eq!(facts.sources().duplicates().len(), duplicates.len());
    for d in duplicates {
        assert!(facts.sources().duplicates().contains(&d));
    }
    for w in facts.workers() {
        for (source, _) in &expected {
            if matches!((&w.source.location, &source.location),
                (SourceLocation::Row(r), SourceLocation::Cell { row, .. }) if r == row)
            {
                assert!(w.fields.iter().any(|f| &f.source == source), "{source:?}");
            }
        }
        for field in &w.fields {
            assert!(
                raw.iter().any(|n| n.source == field.source)
                    || expected.iter().any(|(s, _)| s == &field.source)
            );
        }
    }
}
