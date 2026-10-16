//! Explicit codec for the released child-route spelling. New writes use `sub-track`.
//! Historical evidence and hash preimages remain unchanged.

use crate::report_blocks::{KIND_TASK, flat_text, parse_fence, render_fence, split_body};
use crate::track_report::ReportBlock;
use serde_json::Value;

pub const LEGACY_CHILD_ROUTE: &str = "sub-wave";

pub fn migrate_legacy_payload(payload: &Value) -> Value {
    let mut payload = payload.clone();
    if payload.get("spawn").and_then(Value::as_str) == Some(LEGACY_CHILD_ROUTE) {
        payload["spawn"] = crate::task_recovery::TASK_CHILD_TRACK_ROUTE.into();
    }
    payload
}

/// A typed historical task-block upgrade; prose and unrelated block fields are untouched.
pub fn migrate_legacy_block(block: &ReportBlock) -> ReportBlock {
    if block.kind != KIND_TASK {
        return block.clone();
    }
    ReportBlock {
        payload: migrate_legacy_payload(&block.payload),
        ..block.clone()
    }
}

/// Recipes have no CRDT; replace only a parsed task fence, preserving all other slices verbatim.
pub fn migrate_legacy_body(body: &str) -> String {
    split_body(body)
        .into_iter()
        .map(|slice| {
            let Some(fence) = parse_fence(&slice.raw) else {
                return slice.raw;
            };
            if fence.kind != KIND_TASK {
                return slice.raw;
            }
            let payload = migrate_legacy_payload(&fence.payload);
            if payload == fence.payload {
                slice.raw
            } else {
                render_fence(KIND_TASK, &payload)
            }
        })
        .collect()
}

/// Non-root context references historically hash the whole task fence. Preserve that preimage
/// across this vocabulary-only migration; block/root revisions and execution evidence stay valid.
pub fn context_hash_preimage(block: &ReportBlock) -> String {
    if block.kind == KIND_TASK
        && block.payload.get("spawn").and_then(Value::as_str)
            == Some(crate::task_recovery::TASK_CHILD_TRACK_ROUTE)
    {
        let mut released = block.clone();
        released.payload["spawn"] = LEGACY_CHILD_ROUTE.into();
        flat_text(&released)
    } else {
        flat_text(block)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subtrack_migration_changes_only_task_route_and_keeps_released_hash() {
        let old = ReportBlock {
            id: "b_0001".into(),
            rev: 7,
            kind: KIND_TASK.into(),
            payload: json!({"kind":"codex","spawn":"sub-wave","goal":"sub-wave",
                "context":{"spawn":"sub-wave"}}),
        };
        let new = crate::report_blocks::tasks::normalize_legacy_task_block(&old);
        assert_eq!(new.payload["spawn"], "sub-track");
        assert_eq!(new.payload["goal"], "sub-wave");
        assert_eq!(new.payload["context"], old.payload["context"]);
        assert_eq!((new.id.as_str(), new.rev), (old.id.as_str(), old.rev));
        assert_eq!(context_hash_preimage(&new), flat_text(&old));
        assert_eq!(
            crate::task_recovery::task_root_hash_preimage(&old.payload),
            crate::task_recovery::task_root_hash_preimage(&new.payload)
        );
        let prose = "# sub-wave\n\nLiteral sub-wave stays.\n";
        assert_eq!(migrate_legacy_body(prose), prose);
        let body = format!("{prose}{}", render_fence(KIND_TASK, &old.payload));
        let migrated = migrate_legacy_body(&body);
        assert_eq!(
            migrated,
            format!("{prose}{}", render_fence(KIND_TASK, &new.payload))
        );
        assert_eq!(migrate_legacy_body(&migrated), migrated);
    }
}
