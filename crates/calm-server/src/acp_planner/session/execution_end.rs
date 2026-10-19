//! Retain the existing receipt writer's join result, without a second journal.
use super::*;

enum Completion {
    Running(tokio::task::JoinHandle<()>),
    Finished(std::result::Result<(), ()>),
}

pub(super) struct DriverSlot {
    completion: Completion,
}

impl DriverSlot {
    pub(super) fn new(driver: tokio::task::JoinHandle<()>) -> Self {
        Self {
            completion: Completion::Running(driver),
        }
    }

    async fn finish(&mut self) -> Result<()> {
        if let Completion::Running(driver) = &mut self.completion {
            // Never take a live handle out of its owning slot across an await.
            let result = driver.await.map_err(|_| ());
            self.completion = Completion::Finished(result);
        }
        match &self.completion {
            Completion::Finished(Ok(())) => Ok(()),
            _ => Err(CalmError::Conflict(
                "ACP receipt writer did not finish normally".into(),
            )),
        }
    }
}

pub(super) async fn join_driver(shared: &Shared) -> Result<()> {
    if let Some(driver) = shared.driver.lock().await.as_mut() {
        driver.finish().await?;
    }
    Ok(())
}
