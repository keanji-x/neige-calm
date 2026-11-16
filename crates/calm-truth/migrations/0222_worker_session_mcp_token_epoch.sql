-- Track per-session MCP token epochs so active long-lived sessions (resident ACP,
-- Claude Planner) can deterministically re-derive their current
-- token across harness re-attaches while every revocation advances the epoch.
ALTER TABLE worker_sessions
  ADD COLUMN mcp_token_epoch INTEGER NOT NULL DEFAULT 0;
