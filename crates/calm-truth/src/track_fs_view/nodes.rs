//! Own-track virtual node identity. Classification never inspects content or host paths.
use super::*;

/// A located virtual node, not a promise that its content is present or readable.
/// In particular, implicit run directories need not support `ls`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackFsNodeKind {
    Directory,
    File,
    /// A flat projection and an independently located implicit directory share a spelling.
    FileAndDirectory,
}

impl TrackFsNodeKind {
    /// Whether the spelling has a file identity; cat still performs its final content checks.
    pub fn is_file(self) -> bool {
        matches!(self, Self::File | Self::FileAndDirectory)
    }

    /// Whether the spelling can be a parent, independently of whether it supports ls.
    pub fn is_directory(self) -> bool {
        matches!(self, Self::Directory | Self::FileAndDirectory)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardLeaf {
    Meta,
    Payload,
    Runtime,
    Events,
    Conversation,
    Unknown,
}

impl CardLeaf {
    fn classify(leaf: &str) -> Self {
        match leaf {
            ".meta.json" => Self::Meta,
            ".payload.json" => Self::Payload,
            "runtime.json" => Self::Runtime,
            "events.json" => Self::Events,
            "conversation.md" => Self::Conversation,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProjectionFormat {
    Markdown,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunLeaf<'a> {
    Directory,
    Gates,
    GateLog(&'a str),
    TerminalLog,
    Messages,
    Result,
    Summary,
    Projection {
        format: ProjectionFormat,
        directory: Option<&'a str>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OwnPath<'a> {
    Root,
    Index,
    Track,
    Report,
    Cards,
    CardsIndex(&'a str),
    Card { id: &'a str, leaf: Option<CardLeaf> },
    Runs,
    RunsIndex(&'a str),
    Run { attempt: &'a str, leaf: RunLeaf<'a> },
    // Preserve the existing cat lookup/error order for unrecognized runs paths.
    UnknownRun,
    Unknown,
}

impl<'a> OwnPath<'a> {
    /// Input is normalized by the existing virtual-path owner, never a host path.
    pub(super) fn classify(path: &'a str) -> Self {
        match path {
            "" => Self::Root,
            "index.md" => Self::Index,
            "track.json" => Self::Track,
            "report.md" => Self::Report,
            "cards" => Self::Cards,
            "cards/index.json" => Self::CardsIndex("index.json"),
            "runs" => Self::Runs,
            "runs/index.json" => Self::RunsIndex("index.json"),
            _ => {
                let parts: Vec<_> = path.split('/').collect();
                match parts.as_slice() {
                    ["cards", id] => Self::Card { id, leaf: None },
                    ["cards", id, leaf] => Self::Card {
                        id,
                        leaf: Some(CardLeaf::classify(leaf)),
                    },
                    ["runs", attempt, "terminal-worker.log"] => {
                        Self::run(attempt, RunLeaf::TerminalLog)
                    }
                    ["runs", attempt, "gates", file] => Self::run(attempt, RunLeaf::GateLog(file)),
                    ["runs", attempt, "planner-messages.json"] => {
                        Self::run(attempt, RunLeaf::Messages)
                    }
                    ["runs", attempt, "result"] => Self::run(attempt, RunLeaf::Result),
                    ["runs", attempt, "summary.json"] => Self::run(attempt, RunLeaf::Summary),
                    _ if path.starts_with("runs/") => {
                        // Retain the original projection suffix contract, including legacy keys.
                        let key = path.trim_start_matches("runs/");
                        let directory = match parts.as_slice() {
                            ["runs", attempt] => Some(*attempt),
                            _ => None,
                        };
                        if let Some(key) = key.strip_suffix(".md") {
                            Self::run(
                                key,
                                RunLeaf::Projection {
                                    format: ProjectionFormat::Markdown,
                                    directory,
                                },
                            )
                        } else if let Some(key) = key.strip_suffix(".json") {
                            Self::run(
                                key,
                                RunLeaf::Projection {
                                    format: ProjectionFormat::Json,
                                    directory,
                                },
                            )
                        } else {
                            match parts.as_slice() {
                                ["runs", attempt] => Self::run(attempt, RunLeaf::Directory),
                                ["runs", attempt, "gates"] => Self::run(attempt, RunLeaf::Gates),
                                _ => Self::UnknownRun,
                            }
                        }
                    }
                    _ => Self::Unknown,
                }
            }
        }
    }

    fn run(attempt: &'a str, leaf: RunLeaf<'a>) -> Self {
        Self::Run { attempt, leaf }
    }
}

impl TrackFsView<'_> {
    /// Locate a node under an already-authorized track using the same grammar as cat/ls.
    /// Confirms owner identity and surface capabilities without reading/rendering content.
    /// A file may still lack content (pending result/log); cat retains all final checks.
    pub async fn node_kind(
        &self,
        track: &Track,
        path: &str,
    ) -> Result<TrackFsNodeKind, TrackFsError> {
        use TrackFsNodeKind::{Directory, File};
        let path = normalize_path(path);
        match OwnPath::classify(&path) {
            OwnPath::Root | OwnPath::Cards | OwnPath::Runs => Ok(Directory),
            OwnPath::Index | OwnPath::Track => Ok(File),
            OwnPath::CardsIndex(id) => {
                let directory = self.card_in_track(track, id).await.map(|_| ());
                node_identities(true, directory)
            }
            OwnPath::RunsIndex(id) => {
                node_identities(true, self.run_directory_in_track(track, id, None).await)
            }
            OwnPath::Report => {
                self.report_card_for_track(track).await?;
                Ok(File)
            }
            OwnPath::Card { id, leaf } => {
                self.card_in_track(track, id).await?;
                match leaf {
                    None => Ok(Directory),
                    Some(CardLeaf::Unknown) => Err(path_not_available(&path)),
                    Some(_) => Ok(File),
                }
            }
            OwnPath::Run { attempt, leaf } => {
                match leaf {
                    RunLeaf::GateLog(file) => {
                        let gate = self.execution_gate_attempt(&path, attempt, file)?;
                        let task = self.gate_task_in_track(track, &path, attempt).await?;
                        self.check_gate_run(&task, gate, &path)?;
                    }
                    RunLeaf::Gates => {
                        self.gate_logs_directory(&path)?;
                        task_gate_log_path(attempt, 1)?;
                        let task = self.gate_task_in_track(track, &path, attempt).await?;
                        if task.gate_json.is_none() {
                            return Err(path_not_available(&path));
                        }
                    }
                    RunLeaf::TerminalLog => {
                        self.terminal_log_in_track(track, &path, attempt).await?;
                    }
                    RunLeaf::Projection { directory, .. } => {
                        let runs = self.runs_for_track(track).await?;
                        let file = runs.iter().any(|run| run.attempt_id == attempt);
                        let directory = match directory {
                            Some(id) => self.run_directory_in_track(track, id, Some(&runs)).await,
                            None => Err(path_not_available(&path)),
                        };
                        return node_identities(file, directory);
                    }
                    RunLeaf::Directory => self.run_directory_in_track(track, attempt, None).await?,
                    RunLeaf::Messages | RunLeaf::Result | RunLeaf::Summary => {
                        if leaf == RunLeaf::Summary {
                            self.require_planner_run_summary()?;
                        }
                        let runs = self.runs_for_track(track).await?;
                        run_by_key(&runs, attempt)?;
                    }
                }
                Ok(if matches!(leaf, RunLeaf::Directory | RunLeaf::Gates) {
                    Directory
                } else {
                    File
                })
            }
            OwnPath::UnknownRun | OwnPath::Unknown => Err(path_not_available(&path)),
        }
    }

    async fn run_directory_in_track(
        &self,
        track: &Track,
        attempt: &str,
        runs: Option<&[RunProjection]>,
    ) -> Result<(), TrackFsError> {
        // Task-backed logs can predate the worker event projection.
        if self
            .repo
            .task_get(attempt)
            .await
            .map_err(|e| TrackFsError::Internal(e.to_string()))?
            .is_some()
        {
            self.gate_task_in_track(track, &format!("runs/{attempt}"), attempt)
                .await?;
        } else if let Some(runs) = runs {
            run_by_key(runs, attempt)?;
        } else {
            let runs = self.runs_for_track(track).await?;
            run_by_key(&runs, attempt)?;
        }
        Ok(())
    }

    pub(super) fn require_planner_run_summary(&self) -> Result<(), TrackFsError> {
        if !self.planner_run_summary {
            return Err(TrackFsError::Forbidden(
                "run summary requires a Planner".into(),
            ));
        }
        Ok(())
    }
}

fn node_identities(
    file: bool,
    directory: Result<(), TrackFsError>,
) -> Result<TrackFsNodeKind, TrackFsError> {
    match directory {
        Ok(()) => Ok(if file {
            TrackFsNodeKind::FileAndDirectory
        } else {
            TrackFsNodeKind::Directory
        }),
        // An own file does not authorize a foreign directory with the same spelling.
        Err(TrackFsError::PathNotAvailable(_) | TrackFsError::Forbidden(_)) if file => {
            Ok(TrackFsNodeKind::File)
        }
        Err(error) => Err(error),
    }
}
