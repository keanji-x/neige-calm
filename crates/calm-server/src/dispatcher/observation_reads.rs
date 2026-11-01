//! Read capabilities used while qualifying observations under an existing writer.
//! The backing event repository stays private; consumers cannot open transactions.
use crate::db::{Repo, RepoEventWrite, TrackEvent};
use crate::error::Result;

pub(super) struct ObservationReads<'a> {
    events: &'a dyn RepoEventWrite,
    delivery: Option<sqlx::SqlitePool>,
}
impl<'a> ObservationReads<'a> {
    pub(super) fn new(repo: &'a dyn Repo) -> Self {
        Self {
            events: repo,
            delivery: repo.sqlite_db().map(|db| db.read_pool().clone()),
        }
    }

    pub(super) fn events_only(events: &'a dyn RepoEventWrite) -> Self {
        Self {
            events,
            delivery: None,
        }
    }

    pub(super) async fn task_get(&self, task: &str) -> Result<Option<crate::model::Task>> {
        Ok(self.events.task_get(task).await?)
    }

    pub(super) async fn task_for_worker_card(
        &self,
        card: &str,
    ) -> Result<Option<crate::model::Task>> {
        Ok(self.events.task_for_worker_card(card).await?)
    }

    pub(super) async fn card_role_get(&self, card: &str) -> Result<Option<crate::model::CardRole>> {
        Ok(self.events.card_role_get(card).await?)
    }

    pub(super) async fn report_snapshot(
        &self,
        card: &str,
    ) -> Result<crate::track_report_read::ReportDocSnapshot> {
        crate::track_report_read::load_report_doc_snapshot(self.events, card).await
    }

    pub(super) async fn events_for_track(
        &self,
        track: &str,
        kinds: &[&str],
        since: Option<i64>,
    ) -> Result<Vec<TrackEvent>> {
        Ok(self.events.events_for_track(track, kinds, since).await?)
    }

    pub(super) async fn attempt_has_delivery(&self, task: &str) -> Result<bool> {
        match &self.delivery {
            Some(pool) => crate::git_candidate::delivery::attempt_has_delivery(pool, task).await,
            None => Ok(false),
        }
    }
}
