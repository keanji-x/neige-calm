-- #2058 — a codex or claude task may declare `start: "upstream"`: the kernel fetches the track's
-- upstream, starts the track checkout there and has the worker replay the track's last done
-- commit (a catch-up). `tasks.start` is the declared start, frozen with the row like `access`.
-- Every existing row takes `checkout`, so nothing changes for it.
ALTER TABLE tasks ADD COLUMN start TEXT NOT NULL DEFAULT 'checkout'
  CHECK (start IN ('checkout','upstream'));
