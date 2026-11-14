//! Existing listing and content projections, dispatched by the owner path classifier.
use super::nodes::{CardLeaf, OwnPath, ProjectionFormat, RunLeaf};
use super::*;

impl TrackFsView<'_> {
    pub async fn ls(
        &self,
        track: &Track,
        path: Option<&str>,
    ) -> Result<Vec<TrackFsEntry>, TrackFsError> {
        let path = path.map(normalize_path).unwrap_or_default();
        match OwnPath::classify(&path) {
            OwnPath::Root => {
                let cards = self.cards_for_track(track).await?;
                let runs = self.runs_for_track(track).await?;
                Ok(vec![
                    entry_file("index.md", None, Some(track.updated_at)),
                    entry_file("track.json", None, Some(track.updated_at)),
                    entry_file("report.md", None, Some(track.updated_at)),
                    entry_dir("cards/", Some(cards.len()), None),
                    entry_dir(
                        "runs/",
                        Some(runs.len().saturating_mul(2).saturating_add(1)),
                        runs_updated_at(track, &runs),
                    ),
                ])
            }
            OwnPath::Cards => {
                let cards = self.cards_for_track(track).await?;
                let cards_updated_at = cards_updated_at(track, &cards);
                let mut entries = Vec::with_capacity(cards.len() + 1);
                entries.push(entry_file("index.json", None, Some(cards_updated_at)));
                entries.extend(cards.iter().map(|card| {
                    entry_dir(
                        &format!("{}/", card.id.as_str()),
                        None,
                        Some(card.updated_at),
                    )
                }));
                Ok(entries)
            }
            OwnPath::Runs => {
                let runs = self.runs_for_track(track).await?;
                let mut entries =
                    Vec::with_capacity(runs.len().saturating_mul(2).saturating_add(1));
                entries.push(entry_file(
                    "index.json",
                    None,
                    runs_updated_at(track, &runs),
                ));
                for run in &runs {
                    entries.push(run_listing_entry(run, "md"));
                    entries.push(run_listing_entry(run, "json"));
                }
                Ok(entries)
            }
            OwnPath::Card { id, leaf: None } | OwnPath::CardsIndex(id) => {
                let card = self.card_in_track(track, id).await?;
                let hook_events = self.hook_events_for_card(track, &card.id).await?;
                let hook_events_updated_at = hook_events_updated_at(&card, &hook_events);
                let runtime = self
                    .repo
                    .session_projection_projectable_for_card(&card.id.to_string())
                    .await
                    .map_err(|e| {
                        TrackFsError::Internal(format!("track_file: runtime lookup: {e}"))
                    })?;
                let runtime_updated_at = runtime
                    .as_ref()
                    .map(|runtime| runtime.updated_at_ms)
                    .unwrap_or(card.updated_at);
                Ok(vec![
                    entry_file(".meta.json", None, Some(card.updated_at)),
                    entry_file(".payload.json", None, Some(card.updated_at)),
                    entry_file("runtime.json", None, Some(runtime_updated_at)),
                    entry_file("events.json", None, Some(hook_events_updated_at)),
                    entry_file("conversation.md", None, Some(hook_events_updated_at)),
                ])
            }
            _ => Err(path_not_available(&path)),
        }
    }

    pub async fn cat(&self, track: &Track, path: &str) -> Result<TrackFsContent, TrackFsError> {
        let path = normalize_path(path);
        match OwnPath::classify(&path) {
            OwnPath::Index => {
                let cards = self.cards_for_track(track).await?;
                Ok(content_markdown(index_markdown(track, cards.len())))
            }
            OwnPath::Track => content_json(track),
            OwnPath::Report => {
                let card = self.report_card_for_track(track).await?;
                report_markdown(card.id.as_str(), card.payload)
            }
            OwnPath::CardsIndex(_) => {
                let cards = self.cards_for_track(track).await?;
                let metas: Vec<_> = cards.iter().map(|card| self.card_meta(card)).collect();
                content_json(&metas)
            }
            OwnPath::RunsIndex(_) => {
                let runs = self.runs_for_track(track).await?;
                let summaries: Vec<_> = runs.iter().map(run_index_entry).collect();
                content_json(&summaries)
            }
            OwnPath::Card {
                id,
                leaf: Some(leaf),
            } => {
                let card = self.card_in_track(track, id).await?;
                match leaf {
                    CardLeaf::Meta => content_json(&self.card_meta(&card)),
                    CardLeaf::Payload => content_json(&card.payload),
                    CardLeaf::Runtime => {
                        let runtime = self.runtime_for_card(&card).await?;
                        content_json(&runtime)
                    }
                    CardLeaf::Events => {
                        let hook_events = self.hook_events_for_card(track, &card.id).await?;
                        content_json(&hook_events_json(&hook_events))
                    }
                    CardLeaf::Conversation => {
                        // Prefer the captured worker-flow transcript when populated, else the hook
                        // projection. Page through ALL rows: the db layer clamps `limit` to 500.
                        let rows = worker_flow_rows_all(self.repo, card.id.as_str()).await?;
                        if rows.is_empty() {
                            let hook_events = self.hook_events_for_card(track, &card.id).await?;
                            Ok(content_markdown(conversation_markdown(
                                &card.id,
                                &hook_events,
                            )))
                        } else {
                            // Version-tolerant: a payload this binary cannot deserialize becomes an
                            // `Unknown` placeholder rather than failing the whole read.
                            let items: Vec<calm_types::worker_flow::WorkerFlowItem> = rows
                                .iter()
                                .map(|row| deserialize_flow_row(&row.kind, &row.payload))
                                .collect();
                            Ok(content_markdown(worker_flow_markdown(&card.id, &items)))
                        }
                    }
                    CardLeaf::Unknown => Err(path_not_available(&path)),
                }
            }
            OwnPath::Run { attempt, leaf } => match leaf {
                RunLeaf::TerminalLog => self.cat_terminal_worker_log(track, &path, attempt).await,
                RunLeaf::GateLog(file) => {
                    self.cat_execution_gate_log(track, &path, attempt, file)
                        .await
                }
                RunLeaf::Messages => {
                    let messages = self
                        .repo
                        .planner_messages_for_attempt(track.id.as_str(), attempt)
                        .await
                        .map_err(|e| TrackFsError::Internal(format!("planner messages: {e}")))?;
                    content_json(&messages)
                }
                RunLeaf::Result => self.run_result(track, attempt).await,
                RunLeaf::Summary => {
                    self.require_planner_run_summary()?;
                    let runs = self.runs_for_track(track).await?;
                    self.run_summary(track, run_by_key(&runs, attempt)?).await
                }
                RunLeaf::Projection { format, .. } => {
                    let runs = self.runs_for_track(track).await?;
                    let run = run_by_key(&runs, attempt)?;
                    if format == ProjectionFormat::Markdown {
                        Ok(content_markdown(run_markdown(run)))
                    } else {
                        content_json(&run_json(run))
                    }
                }
                RunLeaf::Directory | RunLeaf::Gates => {
                    self.runs_for_track(track).await?;
                    Err(path_not_available(&path))
                }
            },
            OwnPath::UnknownRun => {
                self.runs_for_track(track).await?;
                Err(path_not_available(&path))
            }
            _ => Err(path_not_available(&path)),
        }
    }
}
