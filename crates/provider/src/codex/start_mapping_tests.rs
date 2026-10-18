//! The neutral request still drives the native production wire builder.
use super::*;
use crate::control::*;

#[test]
fn m1_mapping_turn_request_preserves_native_frame_omissions_and_input() {
    for model in [None, Some("model")] {
        for effort in [None, Some("high")] {
            let request = TurnRequest {
                input: InputRequest {
                    items: vec![InputItem::text("text"), InputItem::local_image("/image")],
                    correlation: CorrelationKey("projection".into()),
                },
                selection: TurnModelSelection {
                    model: model.map(str::to_owned),
                    effort: effort.map(str::to_owned),
                },
                permission: calm_types::harness::PlannerPermissionMode::Never,
                edit: TurnEdit::NoEdit,
            };
            let wire = turn_start_params(
                "bound-thread",
                &request.input.items,
                &request.selection,
                TurnApprovals::Explicit(request.permission),
                Some(&request.input.correlation.0),
            );
            assert_eq!(wire["threadId"], "bound-thread");
            assert_eq!(wire["clientUserMessageId"], "projection");
            assert_eq!(
                wire["input"],
                json!([{"type":"text","text":"text"},{"type":"localImage","path":"/image"}])
            );
            for (key, expected) in [("model", model), ("effort", effort)] {
                assert_eq!(
                    wire.get(key),
                    expected.map(|s| Value::String(s.into())).as_ref()
                );
            }
            assert!(wire.get("edit").is_none());
            assert!(wire.get("permission").is_none());
        }
    }
}
