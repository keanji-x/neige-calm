use super::*;
use crate::model::{Area, AreaKind, Track};
use crate::worker::{
    LivenessTag, SessionMode, WorkerContract, WorkerProviderKind, WorkerSessionState,
};

struct FakeWriteTx {
    root_session_id: Option<WorkerSessionId>,
    worker_session: Option<WorkerSessionRow>,
    worker_session_reads: usize,
    worker_session_read_error: bool,
    /// `cards` rows this fake knows about: id → (role, home track).
    cards: Vec<(CardId, CardRole, TrackId)>,
    /// `tracks` rows this fake knows about: id → area.
    tracks: Vec<(TrackId, AreaId)>,
    bindings: Vec<(CardId, String)>,
    card_read_error: &'static str,
}

impl FakeWriteTx {
    fn new() -> Self {
        Self {
            root_session_id: None,
            worker_session: None,
            worker_session_reads: 0,
            worker_session_read_error: false,
            cards: Vec::new(),
            tracks: Vec::new(),
            bindings: Vec::new(),
            card_read_error: "",
        }
    }

    fn with_card(mut self, card: &str, role: CardRole, track: &str) -> Self {
        self.cards
            .push((CardId::from(card), role, TrackId::from(track)));
        self
    }

    fn with_track(mut self, track: &str, area: &str) -> Self {
        self.tracks.push((TrackId::from(track), AreaId::from(area)));
        self
    }

    fn with_worker_session(worker_session: WorkerSessionRow) -> Self {
        Self {
            worker_session: Some(worker_session),
            ..Self::new()
        }
    }

    fn with_worker_session_read_error() -> Self {
        Self {
            worker_session_read_error: true,
            ..Self::new()
        }
    }
}

impl sealed::Sealed for FakeWriteTx {}

#[async_trait]
impl WriteTx for FakeWriteTx {
    async fn read_track_root_session_id(
        &mut self,
        _track: &TrackId,
    ) -> Result<Option<WorkerSessionId>> {
        Ok(self.root_session_id.clone())
    }

    async fn read_worker_session(
        &mut self,
        id: &WorkerSessionId,
    ) -> Result<Option<WorkerSessionRow>> {
        self.worker_session_reads += 1;
        if self.worker_session_read_error {
            return Err(crate::error::TruthError::Internal(
                "worker session read failed".into(),
            ));
        }
        Ok(self
            .worker_session
            .as_ref()
            .filter(|session| &session.id == id)
            .cloned())
    }

    async fn read_card_role(&mut self, card: &CardId) -> Result<Option<CardRole>> {
        if self.card_read_error == "role" {
            return Err(crate::error::TruthError::Internal(
                "card read failed".into(),
            ));
        }
        Ok(self
            .cards
            .iter()
            .find(|(id, _, _)| id == card)
            .map(|(_, role, _)| *role))
    }

    async fn read_card_track(&mut self, card: &CardId) -> Result<Option<TrackId>> {
        if self.card_read_error == "track" {
            return Err(crate::error::TruthError::Internal(
                "card read failed".into(),
            ));
        }
        Ok(self
            .cards
            .iter()
            .find(|(id, _, _)| id == card)
            .map(|(_, _, track)| track.clone()))
    }

    async fn read_card_binding(
        &mut self,
        card: &CardId,
    ) -> Result<Option<(String, serde_json::Value, Option<String>)>> {
        if self.card_read_error == "binding" {
            return Err(crate::error::TruthError::Internal(
                "binding read failed".into(),
            ));
        }
        Ok(self
            .bindings
            .iter()
            .find(|(id, _)| id == card)
            .map(|(_, session)| ("codex".into(), serde_json::json!({}), Some(session.clone()))))
    }

    async fn read_track_area(&mut self, track: &TrackId) -> Result<Option<AreaId>> {
        Ok(self
            .tracks
            .iter()
            .find(|(id, _)| id == track)
            .map(|(_, area)| area.clone()))
    }
}

fn agent(session_id: &str) -> Principal {
    Principal::Agent {
        session_id: WorkerSessionId::from(session_id),
        track_id: TrackId::from("track-1"),
        area_id: AreaId::from("area-1"),
    }
}

fn worker_session(session_id: &str, card_id: Option<CardId>) -> WorkerSession {
    WorkerSession {
        id: WorkerSessionId::from(session_id),
        track_id: TrackId::from("w"),
        provider: WorkerProviderKind::Codex,
        mode: SessionMode::Resumable,
        contract: WorkerContract::Executor,
        parent_session_id: None,
        requester_session_id: None,
        state: WorkerSessionState::Running,
        mcp_token_hash: None,
        thread_id: None,
        agent_session_id: None,
        active_turn_id: None,
        terminal_run_id: None,
        card_id,
        handle_state_json: None,
        liveness: LivenessTag::Unknown,
        liveness_probed_at_ms: None,
        exit_code: None,
        exit_interpretation: None,
        spawn_op_id: None,
        last_activity_ms: None,
        last_thread_status: None,
        created_at_ms: 0,
        updated_at_ms: 0,
        completed_at_ms: None,
    }
}

fn track(id: &str, area: &str) -> Track {
    Track {
        id: TrackId::from(id),
        area_id: AreaId::from(area),
        title: "t".into(),
        sort: 1.0,
        pinned_at: None,
        closed_at: None,
        admin_granted_at: None,
        cwd_wire_alias: String::new(),
        template_id: None,
        plugin_scope: None,
        purpose: None,
        template_input: None,
        recipe_id: None,
        recipe_revision: None,
        creator_track_id: None,
        creator_key: None,
        workspace: Default::default(),
        created_at: 0,
        updated_at: 0,
    }
}

fn card_scope(card: &str, track: &str, area: &str) -> EventScope {
    EventScope::Card {
        card: CardId::from(card),
        track: TrackId::from(track),
        area: AreaId::from(area),
    }
}

fn track_scope(track: &str, area: &str) -> EventScope {
    EventScope::Track {
        track: TrackId::from(track),
        area: AreaId::from(area),
    }
}

fn track_updated() -> Event {
    Event::TrackUpdated(crate::event::TrackUpdatedPayload::new(
        track("w", "c"),
        None,
    ))
}

fn area_updated() -> Event {
    Event::AreaUpdated(Area {
        id: AreaId::from("c"),
        name: "n".into(),
        color: "#fff".into(),
        sort: 1.0,
        kind: AreaKind::User,
        default_template_id: None,
        default_cwd: None,
        created_at: 0,
        updated_at: 0,
    })
}

fn seeded_caches(card: &CardId, role: CardRole) -> (CardRoleCache, TrackAreaCache) {
    let cache = CardRoleCache::new();
    cache.insert(card.clone(), role, TrackId::from("w"));
    let wcc = TrackAreaCache::new();
    wcc.insert(TrackId::from("w"), AreaId::from("c"));
    (cache, wcc)
}

mod matrix;
mod planner_binding;
mod recorder;
mod sessions;
