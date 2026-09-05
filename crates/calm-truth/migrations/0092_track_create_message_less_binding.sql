-- #1426 — a `POST /api/tracks` that carries no `first_message` may now bind an
-- `Idempotency-Key` too, and such a binding has no message digest to store.
--
-- Version 2 is that shape: the create-request fingerprint is present, the
-- initial-message digest is absent because no message was sent. It is a
-- distinct version rather than "version 1 with a NULL message digest" so the
-- read model can tell the two create shapes apart *positively*. That
-- discrimination is load-bearing: `create_request_sha256` covers the mint
-- inputs only, so a create carrying `first_message` and one omitting it hash
-- the same value. Without the version, the same key could resume a
-- message-carrying create onto a message-less binding and answer 201 for a
-- delivery that never happened. With it, the mismatched shape is a 409
-- `conflict`, in both directions.
--
-- The CHECK is the reason this file exists at all: SQLite cannot alter a table
-- constraint in place, so admitting version 2 means rebuilding the table the
-- way 0089 did. Nothing else about the row changes.
ALTER TABLE track_create_idempotency
RENAME TO track_create_idempotency_0089;

CREATE TABLE track_create_idempotency (
  area_id                     TEXT NOT NULL,
  idempotency_key             TEXT NOT NULL,
  track_id                    TEXT NOT NULL,
  planner_card_id             TEXT NOT NULL,
  report_card_id              TEXT NOT NULL,
  created_at_ms               INTEGER NOT NULL,
  request_fingerprint_version INTEGER NOT NULL,
  create_request_sha256       TEXT,
  first_message_sha256        TEXT,
  PRIMARY KEY (area_id, idempotency_key),
  CHECK (
    (
      request_fingerprint_version = 0
      AND create_request_sha256 IS NULL
      AND first_message_sha256 IS NULL
    )
    OR
    (
      request_fingerprint_version = 1
      AND create_request_sha256 IS NOT NULL
      AND length(create_request_sha256) = 64
      AND create_request_sha256 NOT GLOB '*[^0-9a-f]*'
      AND first_message_sha256 IS NOT NULL
      AND length(first_message_sha256) = 64
      AND first_message_sha256 NOT GLOB '*[^0-9a-f]*'
    )
    OR
    (
      request_fingerprint_version = 2
      AND create_request_sha256 IS NOT NULL
      AND length(create_request_sha256) = 64
      AND create_request_sha256 NOT GLOB '*[^0-9a-f]*'
      AND first_message_sha256 IS NULL
    )
  )
) WITHOUT ROWID;

INSERT INTO track_create_idempotency (
  area_id,
  idempotency_key,
  track_id,
  planner_card_id,
  report_card_id,
  created_at_ms,
  request_fingerprint_version,
  create_request_sha256,
  first_message_sha256
)
SELECT
  area_id,
  idempotency_key,
  track_id,
  planner_card_id,
  report_card_id,
  created_at_ms,
  request_fingerprint_version,
  create_request_sha256,
  first_message_sha256
FROM track_create_idempotency_0089;

DROP TABLE track_create_idempotency_0089;
