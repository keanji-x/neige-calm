//! The live-reply registry on its own: what opens, grows, closes and blocks a reply, and which
//! writer owns a card's entry. The run loop's use of it is pinned in `planner_harness_live_replies`.

use calm_types::harness::{HarnessLiveReplies, HarnessLiveReply};

use super::live_replies::LiveReplies;
use crate::ids::CardId;

fn card() -> CardId {
    CardId::from("card-live".to_string())
}

fn live(turn_id: &str, items: &[(&str, &str)]) -> HarnessLiveReplies {
    HarnessLiveReplies {
        turn_id: Some(turn_id.into()),
        items: items
            .iter()
            .map(|(item_id, text)| HarnessLiveReply {
                item_id: (*item_id).into(),
                text: (*text).into(),
            })
            .collect(),
    }
}

fn nothing() -> HarnessLiveReplies {
    HarnessLiveReplies {
        turn_id: None,
        items: Vec::new(),
    }
}

#[test]
fn a_started_reply_grows_by_its_deltas_and_leaves_once_stored() {
    let replies = LiveReplies::for_test();
    let writer = replies.open(&card(), "ws-1");
    assert_eq!(replies.read(&card()), nothing());
    writer.turn_started("turn-1");
    assert_eq!(replies.read(&card()), live("turn-1", &[]));
    writer.reply_started("turn-1", "item-a");
    writer.delta("turn-1", "item-a", "Hel");
    writer.delta("turn-1", "item-a", "lo");
    writer.reply_started("turn-1", "item-b");
    writer.delta("turn-1", "item-b", "second");
    assert_eq!(
        replies.read(&card()),
        live("turn-1", &[("item-a", "Hello"), ("item-b", "second")])
    );
    writer.item_stored("item-a");
    assert_eq!(
        replies.read(&card()),
        live("turn-1", &[("item-b", "second")])
    );
}

#[test]
fn deltas_and_starts_outside_the_active_turn_are_ignored() {
    let replies = LiveReplies::for_test();
    let writer = replies.open(&card(), "ws-1");
    // No turn yet: nothing opens.
    writer.reply_started("turn-1", "item-a");
    writer.turn_started("turn-1");
    // Never started.
    writer.delta("turn-1", "item-a", "lost");
    // Another turn's start and delta.
    writer.reply_started("turn-0", "item-old");
    writer.delta("turn-0", "item-old", "stale");
    writer.reply_started("turn-1", "item-a");
    writer.delta("turn-0", "item-a", "wrong turn");
    writer.delta("turn-1", "item-a", "kept");
    assert_eq!(replies.read(&card()), live("turn-1", &[("item-a", "kept")]));
}

#[test]
fn a_new_turn_drops_the_old_turns_replies_and_the_same_turn_keeps_them() {
    let replies = LiveReplies::for_test();
    let writer = replies.open(&card(), "ws-1");
    writer.turn_started("turn-1");
    writer.reply_started("turn-1", "item-a");
    writer.delta("turn-1", "item-a", "text");
    writer.turn_started("turn-1");
    assert_eq!(replies.read(&card()), live("turn-1", &[("item-a", "text")]));
    writer.turn_started("turn-2");
    assert_eq!(replies.read(&card()), live("turn-2", &[]));
}

#[test]
fn lost_input_blocks_the_whole_turn_and_the_next_turn_starts_clean() {
    let replies = LiveReplies::for_test();
    let writer = replies.open(&card(), "ws-1");
    writer.turn_started("turn-1");
    writer.reply_started("turn-1", "item-a");
    writer.delta("turn-1", "item-a", "Hel");
    writer.input_lost();
    assert_eq!(replies.read(&card()), live("turn-1", &[]));
    writer.reply_started("turn-1", "item-b");
    writer.delta("turn-1", "item-a", "lo");
    writer.delta("turn-1", "item-b", "x");
    assert_eq!(replies.read(&card()), live("turn-1", &[]));
    assert_eq!(writer.settle("turn-1"), Vec::new());
    assert_eq!(replies.read(&card()), nothing());

    writer.turn_started("turn-2");
    writer.reply_started("turn-2", "item-c");
    writer.delta("turn-2", "item-c", "fresh");
    assert_eq!(
        replies.read(&card()),
        live("turn-2", &[("item-c", "fresh")])
    );
}

#[test]
fn settle_hands_back_the_turns_open_replies_once_and_clears_it() {
    let replies = LiveReplies::for_test();
    let writer = replies.open(&card(), "ws-1");
    writer.turn_started("turn-1");
    writer.reply_started("turn-1", "item-a");
    writer.delta("turn-1", "item-a", "partial");
    assert_eq!(
        writer.settle("turn-0"),
        Vec::new(),
        "another turn settles nothing"
    );
    assert_eq!(
        writer.settle("turn-1"),
        live("turn-1", &[("item-a", "partial")]).items
    );
    assert_eq!(writer.settle("turn-1"), Vec::new());
    assert_eq!(replies.read(&card()), nothing());
}

#[test]
fn a_new_harness_of_the_card_takes_over_and_the_old_writer_changes_nothing() {
    let replies = LiveReplies::for_test();
    let old = replies.open(&card(), "ws-1");
    old.turn_started("turn-1");
    old.reply_started("turn-1", "item-a");
    old.delta("turn-1", "item-a", "old");

    let new = replies.open(&card(), "ws-2");
    assert_eq!(
        replies.read(&card()),
        nothing(),
        "starting a harness clears its card"
    );
    new.turn_started("turn-2");
    old.turn_started("turn-1");
    old.reply_started("turn-1", "item-a");
    old.input_lost();
    drop(old);
    assert_eq!(replies.read(&card()), live("turn-2", &[]));

    drop(new);
    assert_eq!(
        replies.read(&card()),
        nothing(),
        "a run loop's end clears its card"
    );
}
