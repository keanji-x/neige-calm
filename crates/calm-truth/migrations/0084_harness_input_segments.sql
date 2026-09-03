-- Codex reports every turn input as one `userMessage`, even when the kernel
-- built it from several differently-authored observations. Preserve the
-- ordered structured segments so readers never have to classify or split the
-- rendered English. Existing rows stay NULL: their source was already
-- flattened and cannot be reconstructed without guessing from prose.
ALTER TABLE harness_items ADD COLUMN input_segments TEXT
  CHECK (
    input_segments IS NULL OR
    CASE WHEN json_valid(input_segments) THEN
      json_type(input_segments) = 'array' AND json_array_length(input_segments) > 0
    ELSE 0 END
  );
