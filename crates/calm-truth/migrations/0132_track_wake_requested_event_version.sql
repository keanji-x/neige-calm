-- Compiled kernel components ask a Track's Planner to wake with track.wake_requested.
-- Existing databases have no such rows; new events are stamped by the Rust constant.
UPDATE events SET event_version = 23 WHERE kind = 'track.wake_requested';
