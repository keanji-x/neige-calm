-- #1791 — every Planner card names its backend in the server-owned payload key
-- `planner_provider` (the `AgentProvider` serde names `"codex"` / `"claude"`).
-- A Planner card without it is not a harness card, so every existing Planner
-- card, all of which run on Codex, is stamped `"codex"`. Only `role` decides:
-- the legacy `harness`-object payload shape is stamped too.
UPDATE cards
   SET payload = json_set(payload, '$.planner_provider', 'codex')
 WHERE role = 'planner';
