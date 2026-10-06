-- Synchronous publication receipts are distinct from asynchronous PR-open completions.
-- Historical opened events and frozen operations retain their original meaning.
UPDATE events SET event_version = 25 WHERE kind = 'forge.pr.published';
