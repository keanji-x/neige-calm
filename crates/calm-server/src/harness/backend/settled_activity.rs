//! Exact activity observation for a kept input without an addressable turn.
use super::*;
use crate::harness::native_admission::NativePromptAdmission;

pub(crate) enum SettledActivity {
    Settled,
    Held,
}

impl PlannerBackend {
    pub(crate) async fn settled_activity(
        &self,
        card: &str,
        thread: &str,
        admission: &NativePromptAdmission<'_>,
    ) -> Result<SettledActivity> {
        match &self.0 {
            Arm::Codex(daemon, _) => {
                let target = factory::RecoveryTarget::for_settlement(card, thread)?;
                daemon
                    .with_exact_thread(&target, async |native| {
                        Ok(if native.read_settled(admission).await? {
                            SettledActivity::Settled
                        } else {
                            SettledActivity::Held
                        })
                    })
                    .await
            }
            Arm::Claude(_) | Arm::Acp(_) => Ok(SettledActivity::Held),
        }
    }
}
