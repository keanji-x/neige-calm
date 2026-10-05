//! `neige_track_add` (#2104 K1): a Planner opens an ordinary top-level Track from a stored recipe,
//! under a key of its own, and the new row records which Track created it and under which key.
//!
//! The create is the keyed create `POST /api/tracks` runs, reached through
//! [`AppContext::track_creator`]. This module owns who may call it and what the key binds: the
//! caller's role, plugin scope and depth, and the fingerprint of the five inputs.

use crate::error::CalmError;
use crate::ids::{ActorId, CardId};
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    role_gated_write_annotations,
};
use crate::mcp_server::tool_visibility::{TrackPluginScope, plugin_scope_for_track};
use crate::mcp_server::tools::write_args::parse_write_args;
use crate::model::{Card, CardRole, Track};
use crate::session_projection_repo::AgentProvider;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

pub const TOOL_TRACK_ADD: &str = "neige_track_add";

/// Upper bound on `title`, in characters.
const MAX_TITLE_CHARS: usize = 200;
/// Upper bound on `idempotency_key`, in bytes, the same as every keyed route's; it is stored
/// verbatim as `tracks.creator_key`.
const MAX_KEY_BYTES: usize = crate::routes::terminal_cards::IDEMPOTENCY_KEY_MAX_LEN;

/// The tool's five inputs, all required. Exactly these are the request fingerprint.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrackAddArgs {
    pub recipe_id: String,
    pub title: String,
    pub idempotency_key: String,
    /// The first message to the new Track's Planner, verbatim.
    pub text: String,
    /// The audit note carried on the creation `TrackUpdated`.
    pub message: String,
}

/// One authorized call, as the [`TrackCreator`] receives it.
#[derive(Clone, Debug)]
pub struct TrackAddRequest {
    pub creator_track_id: String,
    pub area_id: String,
    /// The calling Planner session; the create transaction's role gate resolves it live.
    pub create_actor: ActorId,
    /// That session's Planner card. The first-message delivery is an operation that may outlive
    /// the session, so it is attributed to the card.
    pub start_actor: ActorId,
    /// Read from the creator's Planner card. Derived, so it stays out of the fingerprint.
    pub planner_provider: AgentProvider,
    pub args: TrackAddArgs,
}

impl TrackAddRequest {
    /// What the key binds: the five inputs, nothing derived from the caller or from current state.
    pub fn fingerprint(&self) -> Result<String, CalmError> {
        crate::routes::terminal_cards::stable_payload_hash(&self.args)
    }
}

/// One open Track a creator added: what the cap refusal lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OpenAddedTrack {
    pub track_id: String,
    pub creator_key: String,
}

/// Why a [`TrackCreator`] created nothing.
#[derive(Debug)]
pub enum TrackAddRefusal {
    /// The creator already holds `cap` open added Tracks.
    OpenCap {
        cap: u32,
        open: Vec<OpenAddedTrack>,
    },
    /// The creator's Planner provider cannot start a new Planner now.
    ProviderUnavailable(String),
    Create(CalmError),
}

impl From<CalmError> for TrackAddRefusal {
    fn from(error: CalmError) -> Self {
        Self::Create(error)
    }
}

/// The keyed create, bound at boot by the route layer that owns it.
#[async_trait::async_trait]
pub trait TrackCreator: Send + Sync {
    /// Mint the Track, or resume the one this request's key already minted.
    async fn add(&self, request: TrackAddRequest) -> Result<Track, TrackAddRefusal>;
}

pub fn register_into(registry: &mut ToolRegistry) {
    registry.register(track_add_descriptor(), wrap(track_add));
}

fn wrap<F, Fut>(f: F) -> ToolHandler
where
    F: Fn(Arc<AppContext>, ToolCallIdentity, Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Value, RpcError>> + Send + 'static,
{
    Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
        let result = f(ctx, identity, args);
        Box::pin(async move {
            result
                .await
                .map(crate::mcp_server::result::ToolResult::structured)
        })
    })
}

fn track_add_descriptor() -> ToolDescriptor {
    // Non-empty is enforced by the handler; the schema stays minimal for the Planner's budget.
    let text = json!({ "type": "string" });
    ToolDescriptor {
        name: TOOL_TRACK_ADD.into(),
        description: include_str!("../../../prompts/tools/neige_track_add.md")
            .trim_end()
            .to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["recipe_id", "title", "idempotency_key", "text", "message"],
            "additionalProperties": false,
            "properties": {
                "recipe_id": text,
                "title": text,
                "idempotency_key": text,
                "text": text,
                "message": text
            }
        }),
        annotations: Some(role_gated_write_annotations()),
        visible_to_roles: &[CardRole::Planner],
    }
}

fn forbidden(reason: impl std::fmt::Display) -> RpcError {
    RpcError::custom(-32403, format!("{TOOL_TRACK_ADD}: forbidden: {reason}"))
}

fn invalid(reason: impl std::fmt::Display) -> RpcError {
    RpcError::invalid_params(format!("{TOOL_TRACK_ADD}: {reason}"))
}

fn internal(reason: impl std::fmt::Display) -> RpcError {
    RpcError::internal(format!("{TOOL_TRACK_ADD}: {reason}"))
}

fn parse_args(args: Value) -> Result<TrackAddArgs, RpcError> {
    // `message` is every write tool's audit note, parsed the one shared way (trimmed, non-empty,
    // `lifecycle` refused); the trimmed note is what the key binds and the event carries.
    let message = parse_write_args(&args, TOOL_TRACK_ADD)?;
    let mut args: TrackAddArgs = serde_json::from_value(args).map_err(invalid)?;
    args.message = message;
    for (name, value) in [
        ("recipe_id", &args.recipe_id),
        ("title", &args.title),
        ("idempotency_key", &args.idempotency_key),
    ] {
        if value.trim().is_empty() {
            return Err(invalid(format!("`{name}` must not be empty")));
        }
    }
    if args.title.chars().count() > MAX_TITLE_CHARS {
        return Err(invalid(format!(
            "`title` is longer than {MAX_TITLE_CHARS} characters"
        )));
    }
    if args.idempotency_key.len() > MAX_KEY_BYTES
        || args
            .idempotency_key
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(invalid(format!(
            "`idempotency_key` must be at most {MAX_KEY_BYTES} bytes with no whitespace"
        )));
    }
    crate::routes::conversations_shared::validate_first_message(&args.text)
        .map_err(|error| invalid(format!("`text`: {error}")))?;
    Ok(args)
}

async fn track_add(
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<Value, RpcError> {
    // Before both arms: a keyed replay resumes without a create transaction, so the in-transaction
    // role gate alone would let any role replay a Planner's request.
    // MUTATION-K1-1: the handler role check.
    if identity.role != CardRole::Planner {
        return Err(forbidden(format!(
            "only a Planner may add a Track; this caller is a {:?}",
            identity.role
        )));
    }
    let args = parse_args(args)?;
    let (card, creator) = resolve_creator(&ctx, &identity).await?;
    if let Some(closed_at) = creator.closed_at {
        return Err(RpcError::custom(
            -32409,
            format!(
                "{TOOL_TRACK_ADD}: Track {} closed at {closed_at}; a closed Track adds none",
                creator.id
            ),
        ));
    }
    let pool = authorize_creator(&ctx, &creator).await?;
    let planner_provider =
        crate::operation::child_track_adapter::track_planner_provider(pool, creator.id.as_str())
            .await
            .map_err(internal)?;
    let request = TrackAddRequest {
        creator_track_id: creator.id.to_string(),
        area_id: creator.area_id.to_string(),
        create_actor: identity.to_actor_id(),
        start_actor: ActorId::AiPlanner(CardId::from(card.id.to_string())),
        planner_provider,
        args,
    };
    let track_creator = ctx
        .track_creator
        .get()
        .and_then(std::sync::Weak::upgrade)
        .ok_or_else(|| internal("no track creator is bound to this server"))?;
    let track = track_creator
        .add(request)
        .await
        .map_err(|refusal| refusal_error(&creator, refusal))?;
    Ok(json!({ "track_id": track.id, "created_at": track.created_at }))
}

/// The creator Track is the caller card's own Track, read live.
async fn resolve_creator(
    ctx: &AppContext,
    identity: &ToolCallIdentity,
) -> Result<(Card, Track), RpcError> {
    let card = ctx
        .repo
        .card_get(&identity.card_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal(format!("card {} not found", identity.card_id)))?;
    let track = ctx
        .repo
        .track_get(card.track_id.as_str())
        .await
        .map_err(internal)?
        .ok_or_else(|| internal(format!("track {} not found", card.track_id)))?;
    Ok((card, track))
}

/// Scope and depth: only an unbound top-level Track that no Track created may add Tracks, so a
/// bound creator cannot escape its plugin fence and an added Track cannot fan out further.
/// Returns the read pool the checks used.
async fn authorize_creator<'a>(
    ctx: &'a Arc<AppContext>,
    creator: &Track,
) -> Result<&'a sqlx::SqlitePool, RpcError> {
    // MUTATION-K1-5: the scope check.
    let scope = plugin_scope_for_track(ctx, Some(creator.id.as_str())).await;
    if scope != TrackPluginScope::All {
        return Err(forbidden(format!(
            "Track {} is bound to a plugin; only an unbound Track may add Tracks",
            creator.id
        )));
    }
    // MUTATION-K1-6: the `creator_track_id` half of the depth check.
    if let Some(by) = &creator.creator_track_id {
        return Err(forbidden(format!(
            "Track {} was added by Track {by}; an added Track adds none",
            creator.id
        )));
    }
    let pool = ctx
        .sqlite_pool
        .as_ref()
        .ok_or_else(|| internal("the creator's depth needs sqlite"))?;
    let parent: Option<String> =
        sqlx::query_scalar("SELECT parent_track_id FROM tracks WHERE id=?1")
            .bind(creator.id.as_str())
            .fetch_one(pool)
            .await
            .map_err(internal)?;
    // MUTATION-K1-7: the `parent_track_id` half of the depth check.
    if let Some(parent) = parent {
        return Err(forbidden(format!(
            "Track {} is a child of Track {parent}; a child Track adds none",
            creator.id
        )));
    }
    Ok(pool)
}

fn refusal_error(creator: &Track, refusal: TrackAddRefusal) -> RpcError {
    match refusal {
        TrackAddRefusal::OpenCap { cap, open } => {
            let listed = open
                .iter()
                .map(|track| format!("{} ({})", track.track_id, track.creator_key))
                .collect::<Vec<_>>()
                .join(", ");
            let message = format!(
                "{TOOL_TRACK_ADD}: Track {} already has {} open Tracks it added, at the \
                 --track-add-max-open cap of {cap}; close one first: {listed}",
                creator.id,
                open.len(),
            );
            // MUTATION-K1-4: `data.open`.
            let data = json!({ "refusal": "open_cap", "cap": cap, "open": open });
            RpcError {
                code: -32409,
                message,
                data: Some(data),
            }
        }
        TrackAddRefusal::ProviderUnavailable(reason) => {
            RpcError::custom(-32503, format!("{TOOL_TRACK_ADD}: {reason}"))
        }
        TrackAddRefusal::Create(error) => match error {
            CalmError::Forbidden(m) => forbidden(m),
            CalmError::Conflict(m)
            | CalmError::IdempotencyKeyExhausted(m)
            | CalmError::IdempotencyKeyReused(m)
            | CalmError::IdempotencyKeyConcurrent(m) => {
                RpcError::custom(-32409, format!("{TOOL_TRACK_ADD}: {m}"))
            }
            CalmError::BadRequest(m) | CalmError::IdempotencyKeyInvalid(m) => invalid(m),
            CalmError::NotFound(m) => RpcError::custom(-32404, format!("{TOOL_TRACK_ADD}: {m}")),
            other => internal(other),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> TrackAddArgs {
        TrackAddArgs {
            recipe_id: "recipe-1".into(),
            title: "US:SPY 研究".into(),
            idempotency_key: "invest-US-SPY-1".into(),
            text: "Start the research.".into(),
            message: "cover SPY".into(),
        }
    }

    fn request(provider: AgentProvider, args: TrackAddArgs) -> TrackAddRequest {
        TrackAddRequest {
            creator_track_id: "track-1".into(),
            area_id: "area-1".into(),
            create_actor: ActorId::AiPlannerSession("session-1".into()),
            start_actor: ActorId::AiPlanner(CardId::from("card-1")),
            planner_provider: provider,
            args,
        }
    }

    /// The fingerprint is the five inputs only: a derived field such as the provider cannot make
    /// a byte-identical retry look like a different request, and every input can.
    #[test]
    fn track_add_fingerprint_ignores_provider() {
        let codex = request(AgentProvider::Codex, args()).fingerprint().unwrap();
        let claude = request(AgentProvider::Claude, args())
            .fingerprint()
            .unwrap();
        assert_eq!(codex, claude, "the provider is derived, not an input");
        let mut other_actor = request(AgentProvider::Codex, args());
        other_actor.create_actor = ActorId::AiPlannerSession("session-2".into());
        other_actor.start_actor = ActorId::AiPlanner(CardId::from("card-2"));
        assert_eq!(other_actor.fingerprint().unwrap(), codex);

        let edits: [fn(&mut TrackAddArgs); 5] = [
            |a| a.recipe_id.push('x'),
            |a| a.title.push('x'),
            |a| a.idempotency_key.push('x'),
            |a| a.text.push('x'),
            |a| a.message.push('x'),
        ];
        for (index, edit) in edits.iter().enumerate() {
            let mut changed = args();
            edit(&mut changed);
            assert_ne!(
                request(AgentProvider::Codex, changed)
                    .fingerprint()
                    .unwrap(),
                codex,
                "input {index} must change the fingerprint"
            );
        }
    }

    #[test]
    fn descriptor_is_planner_only_closed_and_requires_all_five_inputs() {
        let descriptor = track_add_descriptor();
        assert_eq!(descriptor.name, TOOL_TRACK_ADD);
        assert_eq!(descriptor.visible_to_roles, &[CardRole::Planner]);
        assert_eq!(
            descriptor.input_schema["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            descriptor.input_schema["required"],
            json!(["recipe_id", "title", "idempotency_key", "text", "message"])
        );
    }

    #[test]
    fn arguments_are_closed_required_and_bounded() {
        let ok = serde_json::to_value(args()).unwrap();
        assert_eq!(parse_args(ok.clone()).unwrap(), args());
        let mut extra = ok.clone();
        extra["provider"] = json!("claude");
        assert_eq!(
            parse_args(extra).unwrap_err().code,
            RpcError::INVALID_PARAMS
        );
        for key in ["recipe_id", "title", "idempotency_key", "text", "message"] {
            let mut missing = ok.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert_eq!(
                parse_args(missing).unwrap_err().code,
                RpcError::INVALID_PARAMS,
                "{key} is required"
            );
            let mut blank = ok.clone();
            blank[key] = json!("  ");
            assert_eq!(
                parse_args(blank).unwrap_err().code,
                RpcError::INVALID_PARAMS,
                "{key} must not be blank"
            );
        }
        let mut spaced = ok.clone();
        spaced["idempotency_key"] = json!("a key");
        assert_eq!(
            parse_args(spaced).unwrap_err().code,
            RpcError::INVALID_PARAMS
        );
        // `message` takes the shared write-args path: trimmed, and `lifecycle` named and refused.
        let mut padded = ok.clone();
        padded["message"] = json!("  cover SPY \n");
        assert_eq!(parse_args(padded).unwrap().message, "cover SPY");
        let mut lifecycle = ok;
        lifecycle["lifecycle"] = json!("done");
        let refused = parse_args(lifecycle).unwrap_err();
        assert!(
            refused.message.contains("`lifecycle` is removed"),
            "{refused:?}"
        );
    }
}
