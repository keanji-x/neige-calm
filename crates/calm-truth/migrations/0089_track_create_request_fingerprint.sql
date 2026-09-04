-- #1434 — bind the original create request in the same durable row as the
-- track id. The operation payload_hash cannot be that authority: the exact
-- daemon-outage window this table was introduced for has no operation row.
--
-- Version 0 is an explicit legacy state for rows written by migration 0088's
-- code. Their original request cannot be reconstructed: an operation may not
-- exist, and even when one does its old digest omitted several mint inputs.
-- New writers may only write version 1 with both complete SHA-256 values.
ALTER TABLE track_create_idempotency
RENAME TO track_create_idempotency_0088;

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
  0,
  NULL,
  NULL
FROM track_create_idempotency_0088;

DROP TABLE track_create_idempotency_0088;
