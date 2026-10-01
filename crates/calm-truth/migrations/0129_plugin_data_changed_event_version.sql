-- Calendar and future builtin components publish opaque plugin data invalidation.
-- Existing databases have no such rows; new events are stamped by the Rust constant.
UPDATE events SET event_version = 22 WHERE kind = 'plugin.data.changed';
