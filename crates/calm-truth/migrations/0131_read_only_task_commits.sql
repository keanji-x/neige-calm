-- #1933 — a read-only task may declare `head` (the commit the track checkout must be at when the
-- task starts; the kernel refuses the launch otherwise) and `base` (the commit a review compares
-- against). Both are full commit ids, frozen with the row like `access`. Every existing row takes
-- NULL, so nothing changes for it.
ALTER TABLE tasks ADD COLUMN head TEXT NULL
  CHECK (head IS NULL OR (length(head) = 40 AND head NOT GLOB '*[^0-9a-f]*'));
ALTER TABLE tasks ADD COLUMN base TEXT NULL
  CHECK (base IS NULL OR (length(base) = 40 AND base NOT GLOB '*[^0-9a-f]*'));
