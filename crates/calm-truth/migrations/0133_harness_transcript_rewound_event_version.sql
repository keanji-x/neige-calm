-- A rewind (#1923) removes a conversation's latest turn and announces it with harness.transcript.rewound.
-- Existing databases have no such rows; new events are stamped by the Rust constant.
UPDATE events SET event_version = 24 WHERE kind = 'harness.transcript.rewound';
