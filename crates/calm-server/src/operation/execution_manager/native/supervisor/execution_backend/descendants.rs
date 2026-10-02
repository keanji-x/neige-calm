//! A turn's lifecycle includes provider-spawned descendant threads.
//! Pagination and ancestry come from the 0.159.2 protocol, never local caches.
use super::*;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_ROSTER_PAGES: usize = 64;
#[derive(Clone, PartialEq, Eq)]
struct Member {
    parent: String,
    cwd: String,
    depth: u64,
}
impl CodexBackend {
    pub(super) async fn descendants_stopped(
        &self,
        root: &str,
        cwd: &str,
        stop: bool,
    ) -> Result<bool> {
        #[cfg(feature = "fixtures")]
        if self.fake.is_some() {
            return Ok(true);
        }
        let client = self.client()?;
        let before = self.descendant_roster(root, cwd).await?;
        let loaded = self.loaded_roster().await?;
        // A loaded descendant absent from the ancestor query invalidates completeness.
        self.verify_loaded_ancestry(root, &before, &loaded).await?;
        for (id, member) in &before {
            let raw = client.thread_read_full(id).await?.thread;
            verify_member(id, member, cwd, &raw)?;
            let facts: super::super::workspace::NativeThread = serde_json::from_value(raw)
                .map_err(|error| CalmError::CodexAppServer(format!("descendant facts: {error}")))?;
            if stop {
                for turn in &facts.turns {
                    if matches!(turn.status, TurnStatus::InProgress) {
                        self.interrupt_provider(id, &turn.id).await?;
                    } else if matches!(turn.status, TurnStatus::Unknown) {
                        return Ok(false);
                    }
                }
                client.clean_background_terminals(id).await?;
            }
        }
        let after = self.descendant_roster(root, cwd).await?;
        if before != after {
            return Ok(false);
        }
        let loaded = self.loaded_roster().await?;
        self.verify_loaded_ancestry(root, &after, &loaded).await?;
        for (id, member) in &after {
            let raw = client.thread_read_full(id).await?.thread;
            verify_member(id, member, cwd, &raw)?;
            let facts: super::super::workspace::NativeThread = serde_json::from_value(raw)
                .map_err(|error| {
                    CalmError::CodexAppServer(format!("descendant stop facts: {error}"))
                })?;
            let terminal = facts.turns.iter().all(|turn| {
                matches!(
                    turn.status,
                    TurnStatus::Completed | TurnStatus::Interrupted | TurnStatus::Failed
                )
            });
            let inactive = matches!(facts.status, ThreadStatus::Idle | ThreadStatus::SystemError)
                || (matches!(facts.status, ThreadStatus::NotLoaded) && !loaded.contains(id));
            if !terminal || !inactive || !client.background_terminals_stopped(id).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    async fn descendant_roster(&self, root: &str, cwd: &str) -> Result<BTreeMap<String, Member>> {
        let mut members = BTreeMap::new();
        for archived in [false, true] {
            let mut cursor: Option<String> = None;
            let mut cursors = BTreeSet::new();
            for _ in 0..MAX_ROSTER_PAGES {
                let page = self
                    .client()?
                    .descendants_page(root, cursor.as_deref(), archived)
                    .await?;
                let data = page
                    .get("data")
                    .and_then(Value::as_array)
                    .ok_or_else(|| CalmError::Conflict("descendant roster is missing".into()))?;
                for thread in data {
                    let id = thread
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .ok_or_else(|| {
                            CalmError::Conflict("descendant identity is missing".into())
                        })?;
                    let parent = parent(thread)?.ok_or_else(|| {
                        CalmError::Conflict("descendant ancestry is missing".into())
                    })?;
                    let path = thread
                        .get("cwd")
                        .and_then(Value::as_str)
                        .ok_or_else(|| CalmError::Conflict("descendant cwd is missing".into()))?;
                    let canonical = std::fs::canonicalize(path)?;
                    if !canonical.starts_with(std::fs::canonicalize(cwd)?) || id == root {
                        return Err(CalmError::Conflict(
                            "descendant differs from the frozen execution scope".into(),
                        ));
                    }
                    let depth = thread
                        .pointer("/source/subAgent/thread_spawn/depth")
                        .and_then(Value::as_u64)
                        .filter(|depth| *depth > 0)
                        .ok_or_else(|| CalmError::Conflict("descendant depth is missing".into()))?;
                    let member = Member {
                        parent: parent.into(),
                        depth,
                        cwd: canonical
                            .to_str()
                            .ok_or_else(|| {
                                CalmError::Conflict("descendant cwd is not UTF-8".into())
                            })?
                            .into(),
                    };
                    if members.insert(id.into(), member).is_some() {
                        return Err(CalmError::Conflict(
                            "descendant roster repeats an identity".into(),
                        ));
                    }
                }
                cursor = next_cursor(&page)?;
                if cursor.is_none() {
                    break;
                }
                if !cursors.insert(cursor.clone()) {
                    break;
                }
            }
            if cursor.is_some() {
                return Err(CalmError::Conflict(
                    "descendant pagination is incomplete".into(),
                ));
            }
        }
        verify_chains(root, &members)?;
        Ok(members)
    }
    async fn loaded_roster(&self) -> Result<BTreeSet<String>> {
        let mut loaded = BTreeSet::new();
        let mut cursor: Option<String> = None;
        let mut cursors = BTreeSet::new();
        for _ in 0..MAX_ROSTER_PAGES {
            let page = self.client()?.loaded_page(cursor.as_deref()).await?;
            let data = page
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| CalmError::Conflict("loaded roster is missing".into()))?;
            for id in data {
                let id = id
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| CalmError::Conflict("loaded identity is missing".into()))?;
                if !loaded.insert(id.into()) {
                    return Err(CalmError::Conflict(
                        "loaded roster repeats an identity".into(),
                    ));
                }
            }
            cursor = next_cursor(&page)?;
            if cursor.is_none() {
                return Ok(loaded);
            }
            if !cursors.insert(cursor.clone()) {
                break;
            }
        }
        Err(CalmError::Conflict(
            "loaded pagination is incomplete".into(),
        ))
    }
    async fn verify_loaded_ancestry(
        &self,
        root: &str,
        members: &BTreeMap<String, Member>,
        loaded: &BTreeSet<String>,
    ) -> Result<()> {
        for id in loaded {
            if id == root {
                continue;
            }
            let raw = self.client()?.thread_read_full(id).await?.thread;
            if raw.get("id").and_then(Value::as_str) != Some(id) {
                return Err(CalmError::Conflict("loaded thread identity changed".into()));
            }
            let mut ancestor = parent(&raw)?.map(str::to_owned);
            let mut seen = BTreeSet::new();
            while let Some(current) = ancestor {
                if current == root || members.contains_key(&current) {
                    if !members.contains_key(id) {
                        return Err(CalmError::Conflict(
                            "ancestor roster missed a loaded descendant".into(),
                        ));
                    }
                    break;
                }
                if !seen.insert(current.clone()) || seen.len() > MAX_ROSTER_PAGES * 100 {
                    return Err(CalmError::Conflict(
                        "loaded ancestry is cyclic or incomplete".into(),
                    ));
                }
                let facts = self.client()?.thread_read_full(&current).await?.thread;
                if facts.get("id").and_then(Value::as_str) != Some(current.as_str()) {
                    return Err(CalmError::Conflict(
                        "loaded ancestor identity changed".into(),
                    ));
                }
                ancestor = parent(&facts)?.map(str::to_owned);
            }
        }
        Ok(())
    }
}
fn next_cursor(page: &Value) -> Result<Option<String>> {
    match page.get("nextCursor") {
        Some(Value::Null) | None => Ok(None),
        Some(Value::String(cursor)) if !cursor.is_empty() => Ok(Some(cursor.clone())),
        _ => Err(CalmError::Conflict("invalid provider roster cursor".into())),
    }
}
fn parent(thread: &Value) -> Result<Option<&str>> {
    let source = thread
        .get("source")
        .ok_or_else(|| CalmError::Conflict("thread ancestry facts are missing".into()))?;
    if let Some(parent) = source
        .pointer("/subAgent/thread_spawn/parent_thread_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        return Ok(Some(parent));
    }
    if matches!(
        source.as_str(),
        Some("cli" | "vscode" | "exec" | "appServer")
    ) || source.get("custom").and_then(Value::as_str).is_some()
    {
        return Ok(None);
    }
    Err(CalmError::Conflict(
        "thread ancestry remains unknown".into(),
    ))
}
fn verify_chains(root: &str, members: &BTreeMap<String, Member>) -> Result<()> {
    for id in members.keys() {
        let mut node = id.as_str();
        let mut seen = BTreeSet::new();
        while node != root {
            if !seen.insert(node) {
                return Err(CalmError::Conflict(
                    "descendant ancestry contains a cycle".into(),
                ));
            }
            let child = members.get(node).ok_or_else(|| {
                CalmError::Conflict("descendant ancestry leaves the owned tree".into())
            })?;
            if let Some(parent) = members.get(&child.parent) {
                if parent.depth.checked_add(1) != Some(child.depth) {
                    return Err(CalmError::Conflict(
                        "descendant depth contradicts its ancestry".into(),
                    ));
                }
            }
            node = child.parent.as_str();
        }
    }
    Ok(())
}
fn verify_member(id: &str, member: &Member, root_cwd: &str, thread: &Value) -> Result<()> {
    let cwd = thread
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or_else(|| CalmError::Conflict("descendant stop cwd is missing".into()))?;
    if thread.get("id").and_then(Value::as_str) != Some(id)
        || parent(thread)? != Some(member.parent.as_str())
        || thread
            .pointer("/source/subAgent/thread_spawn/depth")
            .and_then(Value::as_u64)
            != Some(member.depth)
        || std::fs::canonicalize(cwd)? != std::path::Path::new(&member.cwd)
        || !std::fs::canonicalize(cwd)?.starts_with(std::fs::canonicalize(root_cwd)?)
    {
        return Err(CalmError::Conflict(
            "descendant stop facts changed ownership or scope".into(),
        ));
    }
    Ok(())
}
