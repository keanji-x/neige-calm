//! The live-reply registry on its own: what opens, grows, closes and blocks a reply, and which
//! writer owns a card's entry. The run loop's use of it is pinned in `planner_harness_live_replies`.

use std::sync::Arc;

use calm_types::harness::{HarnessLiveReplies, HarnessLiveReply};
use serde_json::{Value, json};

use super::live_replies::{LiveReplies, LiveReplyWriter, OpenReply};
use crate::ids::CardId;

fn card() -> CardId {
    CardId::from("card-live".to_string())
}

/// The writer of a harness the registry installed.
fn installed(replies: &Arc<LiveReplies>, worker_session_id: &str) -> LiveReplyWriter {
    let claim = replies.claim(&card(), worker_session_id);
    claim.install();
    claim.writer()
}

fn started(item_id: &str) -> Value {
    json!({ "id": item_id, "type": "agentMessage", "phase": "final_answer", "text": "" })
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
    let writer = installed(&replies, "ws-1");
    assert_eq!(replies.read(&card()), nothing());
    writer.turn_started("turn-1");
    assert_eq!(replies.read(&card()), live("turn-1", &[]));
    writer.reply_started("turn-1", "item-a", &started("item-a"));
    writer.delta("turn-1", "item-a", "Hel");
    writer.delta("turn-1", "item-a", "lo");
    writer.reply_started("turn-1", "item-b", &started("item-b"));
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
    let writer = installed(&replies, "ws-1");
    // No turn yet: nothing opens.
    writer.reply_started("turn-1", "item-a", &started("item-a"));
    writer.turn_started("turn-1");
    // Never started.
    writer.delta("turn-1", "item-a", "lost");
    // Another turn's start and delta.
    writer.reply_started("turn-0", "item-old", &started("item-old"));
    writer.delta("turn-0", "item-old", "stale");
    writer.reply_started("turn-1", "item-a", &started("item-a"));
    writer.delta("turn-0", "item-a", "wrong turn");
    writer.delta("turn-1", "item-a", "kept");
    assert_eq!(replies.read(&card()), live("turn-1", &[("item-a", "kept")]));
}

#[test]
fn a_new_turn_drops_the_old_turns_replies_and_the_same_turn_keeps_them() {
    let replies = LiveReplies::for_test();
    let writer = installed(&replies, "ws-1");
    writer.turn_started("turn-1");
    writer.reply_started("turn-1", "item-a", &started("item-a"));
    writer.delta("turn-1", "item-a", "text");
    writer.turn_started("turn-1");
    assert_eq!(replies.read(&card()), live("turn-1", &[("item-a", "text")]));
    writer.turn_started("turn-2");
    assert_eq!(replies.read(&card()), live("turn-2", &[]));
}

#[test]
fn lost_input_blocks_the_whole_turn_and_the_next_turn_starts_clean() {
    let replies = LiveReplies::for_test();
    let writer = installed(&replies, "ws-1");
    writer.turn_started("turn-1");
    writer.reply_started("turn-1", "item-a", &started("item-a"));
    writer.delta("turn-1", "item-a", "Hel");
    writer.input_lost();
    assert_eq!(replies.read(&card()), live("turn-1", &[]));
    writer.reply_started("turn-1", "item-b", &started("item-b"));
    writer.delta("turn-1", "item-a", "lo");
    writer.delta("turn-1", "item-b", "x");
    assert_eq!(replies.read(&card()), live("turn-1", &[]));
    assert_eq!(writer.settle("turn-1"), Vec::new());
    assert_eq!(replies.read(&card()), nothing());

    writer.turn_started("turn-2");
    writer.reply_started("turn-2", "item-c", &started("item-c"));
    writer.delta("turn-2", "item-c", "fresh");
    assert_eq!(
        replies.read(&card()),
        live("turn-2", &[("item-c", "fresh")])
    );
}

#[test]
fn settle_hands_back_the_open_replies_once_and_each_stays_live_until_stored() {
    let replies = LiveReplies::for_test();
    let writer = installed(&replies, "ws-1");
    writer.turn_started("turn-1");
    writer.reply_started("turn-1", "item-a", &started("item-a"));
    writer.delta("turn-1", "item-a", "partial");
    writer.reply_started("turn-1", "item-b", &started("item-b"));
    writer.delta("turn-1", "item-b", "second");
    assert_eq!(
        writer.settle("turn-0"),
        Vec::new(),
        "another turn settles nothing"
    );
    let open = |item_id: &str, text: &str| OpenReply {
        item_id: item_id.into(),
        started: started(item_id),
        text: text.into(),
    };
    assert_eq!(
        writer.settle("turn-1"),
        [open("item-a", "partial"), open("item-b", "second")]
    );
    assert_eq!(writer.settle("turn-1"), Vec::new(), "a turn settles once");
    // A settled turn takes no more text or replies.
    writer.delta("turn-1", "item-a", " more");
    writer.reply_started("turn-1", "item-c", &started("item-c"));
    let both = live("turn-1", &[("item-a", "partial"), ("item-b", "second")]);
    assert_eq!(
        replies.read(&card()),
        both,
        "nothing leaves before it is stored"
    );
    writer.item_stored("item-a");
    assert_eq!(
        replies.read(&card()),
        live("turn-1", &[("item-b", "second")])
    );
    writer.item_stored("item-b");
    assert_eq!(
        replies.read(&card()),
        nothing(),
        "the last stored reply clears the turn"
    );
}

#[test]
fn a_reply_that_is_never_stored_stays_until_the_next_turn_starts() {
    let replies = LiveReplies::for_test();
    let writer = installed(&replies, "ws-1");
    writer.turn_started("turn-1");
    writer.reply_started("turn-1", "item-a", &started("item-a"));
    writer.delta("turn-1", "item-a", "kept");
    assert_eq!(writer.settle("turn-1").len(), 1);
    assert_eq!(replies.read(&card()), live("turn-1", &[("item-a", "kept")]));
    writer.turn_started("turn-2");
    assert_eq!(replies.read(&card()), live("turn-2", &[]));
}

#[test]
fn discard_clears_the_ended_turn_and_nothing_else() {
    let replies = LiveReplies::for_test();
    let writer = installed(&replies, "ws-1");
    writer.turn_started("turn-1");
    writer.reply_started("turn-1", "item-a", &started("item-a"));
    writer.discard("turn-0");
    assert_eq!(replies.read(&card()), live("turn-1", &[("item-a", "")]));
    writer.discard("turn-1");
    assert_eq!(replies.read(&card()), nothing());
}

/// A start that lost to a concurrent one built its harness but was never installed.
#[test]
fn a_writer_whose_harness_was_never_installed_changes_nothing() {
    let replies = LiveReplies::for_test();
    let winner = installed(&replies, "ws-1");
    let loser = replies.claim(&card(), "ws-1").writer();
    winner.turn_started("turn-1");
    loser.turn_started("turn-x");
    loser.input_lost();
    drop(loser);
    winner.reply_started("turn-1", "item-a", &started("item-a"));
    winner.delta("turn-1", "item-a", "winner");
    assert_eq!(
        replies.read(&card()),
        live("turn-1", &[("item-a", "winner")])
    );
}

#[test]
fn a_new_harness_of_the_card_takes_over_and_the_old_writer_changes_nothing() {
    let replies = LiveReplies::for_test();
    let old = installed(&replies, "ws-1");
    old.turn_started("turn-1");
    old.reply_started("turn-1", "item-a", &started("item-a"));
    old.delta("turn-1", "item-a", "old");

    let new = installed(&replies, "ws-2");
    assert_eq!(
        replies.read(&card()),
        nothing(),
        "starting a harness clears its card"
    );
    new.turn_started("turn-2");
    old.turn_started("turn-1");
    old.reply_started("turn-1", "item-a", &started("item-a"));
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
