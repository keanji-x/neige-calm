//! Admitted canonical maintenance; no healing or retry after a native write.
use super::*;
use crate::harness::backend::{MaintenanceObservation, MaintenanceTarget};

impl SharedCodexAppServer {
    pub(crate) async fn thread_compact_delivery_admitted(
        &self,
        target: MaintenanceTarget,
        admission: &NativePromptAdmission<'_>,
    ) -> CompactDelivery<ControlError> {
        let refused = |error| CompactDelivery::NotWritten {
            error: ControlError::Unanswered(error),
        };
        if let Err(error) = self.require_authentication() {
            return CompactDelivery::NotWritten { error };
        }
        let thread = &target.thread_id;
        let client = match self.authenticated_client().await {
            Ok(client) => client,
            Err(error) => return CompactDelivery::NotWritten { error },
        };
        let ticket = match self.native_turns.prepare_start(
            &client,
            thread,
            NativeInput::Maintenance(target.clone()),
        ) {
            Ok(ticket) => ticket,
            Err(error) => return refused(error),
        };
        let gate = admission.gate_with_final_check(|| {
            self.require_authentication().map_err(CalmError::from)?;
            if self.thread_seals.is_sealed(thread) {
                return Err(CalmError::Conflict(
                    "the conversation is being deleted".into(),
                ));
            }
            self.native_turns.admit_start(&client, thread, &ticket)
        });
        match client.compact_delivery_gated(thread, &gate).await {
            CompactDelivery::NativeAck => CompactDelivery::NativeAck,
            CompactDelivery::NotWritten { error } => {
                let clean = self.native_turns.not_started(&client, thread, &ticket);
                let error = ControlError::Unanswered(
                    admission
                        .take_refusal()
                        .unwrap_or_else(|| error.into_legacy_error().into()),
                );
                if clean {
                    CompactDelivery::NotWritten { error }
                } else {
                    CompactDelivery::Unknown { error }
                }
            }
            CompactDelivery::Unknown { error } => CompactDelivery::Unknown {
                error: error.into(),
            },
        }
    }

    pub(crate) fn observe_maintenance(
        &self,
        target: &MaintenanceTarget,
    ) -> Option<MaintenanceObservation> {
        self.native_turns.maintenance(target)
    }
}
