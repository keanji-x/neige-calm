-- PR9a read-flip: back the Cohort-B hot-path session lookups (flipped in 9a-ii) and the
-- card<->session reverse-JOIN (card_identity_get_by_session). Additive; no read flipped yet.
CREATE INDEX IF NOT EXISTS ws_provider_thread_idx
    ON worker_sessions(provider, thread_id) WHERE thread_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ws_provider_session_idx
    ON worker_sessions(provider, agent_session_id) WHERE agent_session_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS cards_session_idx
    ON cards(session_id) WHERE session_id IS NOT NULL;
