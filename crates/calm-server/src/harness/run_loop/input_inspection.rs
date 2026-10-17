//! A read on the queue owner's command loop, after an entire drain/mutation has settled.
//! No second snapshot, inferred phase, or provider-facing operation is involved.
use super::{
    HarnessObservationCommand, Inner, ObservationIngress, PlannerHarness, is_user_message_type,
};
use crate::error::Result;
use crate::harness::queue::{QueueEntry, QueueEntryId};
use crate::model::HarnessInputPresentation;
use calm_types::conversation_receipt::InitialInputUnavailableReason;
use std::sync::{Arc, atomic::Ordering};
use tokio::sync::oneshot;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InputInspection {
    Queued {
        entry_id: String,
        rev: u32,
        text: String,
    },
    Projected {
        item_id: i64,
        segment_index: u32,
        entry_rev: u32,
    },
    Unavailable(InitialInputUnavailableReason),
}

impl PlannerHarness {
    pub(crate) fn is_shutting_down(&self) -> bool {
        self.inner.shutting_down.load(Ordering::SeqCst)
    }

    pub(crate) async fn inspect_input(&self, entry_id: QueueEntryId) -> Result<InputInspection> {
        if self.is_shutting_down() {
            return Ok(InputInspection::Unavailable(
                InitialInputUnavailableReason::ShuttingDown,
            ));
        }
        let result = match &self.inner.observations {
            ObservationIngress::Running(sender) => {
                let (answer, reply) = oneshot::channel();
                if sender
                    .try_send(HarnessObservationCommand::InspectInput { entry_id, answer })
                    .is_err()
                {
                    return Ok(InputInspection::Unavailable(
                        InitialInputUnavailableReason::RuntimeUnavailable,
                    ));
                }
                match reply.await {
                    Ok(result) => result?,
                    Err(_) => {
                        return Ok(InputInspection::Unavailable(
                            InitialInputUnavailableReason::ShuttingDown,
                        ));
                    }
                }
            }
            #[cfg(feature = "fixtures")]
            ObservationIngress::Unstarted(_) => inspect(&self.inner, &entry_id).await?,
        };
        if self.is_shutting_down() {
            return Ok(InputInspection::Unavailable(
                InitialInputUnavailableReason::ShuttingDown,
            ));
        }
        Ok(result)
    }
}

pub(super) async fn inspect(
    inner: &Arc<Inner>,
    entry_id: &QueueEntryId,
) -> Result<InputInspection> {
    let queue = inner.pending_queue.lock().await;
    let mut matches = queue
        .iter()
        .filter_map(|entry| entry.user_view())
        .filter(|entry| entry.id == entry_id);
    if let Some(entry) = matches.next() {
        if matches.next().is_some() {
            return Ok(InputInspection::Unavailable(
                InitialInputUnavailableReason::Integrity,
            ));
        }
        // A failed projection deletion can leave both representations. The queue is current.
        return Ok(InputInspection::Queued {
            entry_id: entry.id.to_string(),
            rev: entry.rev,
            text: entry.text.to_string(),
        });
    }
    let mut legacy = queue
        .iter()
        .any(|entry| matches!(entry, QueueEntry::LegacyUser { .. }));
    drop(queue);

    // Accepted steers can have no row. Their real entry revision still bounds
    // what older retained history can prove while this owner holds the input.
    let mut latest: Option<(u32, Option<(i64, u32)>)> = inner
        .steered_into_running_turn
        .lock()
        .await
        .iter()
        .filter_map(|steered| steered.entry.user_view())
        .filter(|entry| entry.id == entry_id)
        .map(|entry| entry.rev)
        .max()
        .map(|rev| (rev, None));
    let mut before = 0;
    loop {
        let page = inner
            .repo
            .harness_item_list_transcript_by_card(inner.card_id.as_str(), before, 128, true)
            .await?;
        if page.is_empty() {
            break;
        }
        before = page[0].id;
        for item in page.iter().rev() {
            if item.method != "item/completed" || !is_user_message_type(item.item_type.as_deref()) {
                continue;
            }
            let Some(segments) = &item.input_segments else {
                legacy = true;
                continue;
            };
            for (index, segment) in segments.iter().enumerate() {
                if segment.presentation != HarnessInputPresentation::User {
                    continue;
                }
                // TrackGoal is human-presented but is not an addressable user queue entry.
                // Its declared origin proves it cannot be the receipt we are looking for.
                if segment
                    .origin
                    .as_ref()
                    .is_some_and(|origin| origin.observation != "user_message")
                {
                    continue;
                }
                match segment
                    .origin
                    .as_ref()
                    .and_then(|origin| origin.queue_entry.as_ref())
                {
                    Some(entry) if entry.id == entry_id.as_str() => {
                        // The drain writes before turn/start, with no turn binding. Cleanup
                        // can fail, and recovery can discard the old owner's uncertainty.
                        // Only the provider's completed user-message upgrade binds this row
                        // to a real turn while preserving these exact entry segments.
                        let bound = item.turn_id.as_ref().map(|_| (item.id, index as u32));
                        match latest.as_mut() {
                            Some((rev, known)) if *rev == entry.rev => {
                                if known.is_none() {
                                    *known = bound;
                                }
                            }
                            Some((rev, _)) if *rev > entry.rev => {}
                            _ => latest = Some((entry.rev, bound)),
                        }
                    }
                    Some(_) => {}
                    None => legacy = true,
                }
            }
        }
    }
    if let Some((entry_rev, Some((item_id, segment_index)))) = latest {
        return Ok(InputInspection::Projected {
            item_id,
            segment_index,
            entry_rev,
        });
    }
    Ok(if legacy {
        InputInspection::Unavailable(InitialInputUnavailableReason::LegacyProjection)
    } else {
        // An accepted steer may have no row, and its live-only evidence does not
        // survive hydration. Empty queue and history cannot certify cancellation.
        InputInspection::Unavailable(InitialInputUnavailableReason::RuntimeUnavailable)
    })
}
