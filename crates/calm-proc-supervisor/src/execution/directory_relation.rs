use super::{directory_probe::*, *};
use workspace::execution::objects::*;

impl Executor {
    pub(super) async fn observe_directories(
        self: &Arc<Self>,
        request: DirectoryRelationRequest,
    ) -> anyhow::Result<ExecutionReply> {
        anyhow::ensure!(request.executor_boot == self.boot, "observer boot mismatch");
        let roots = {
            let live = self.live_objects.lock().map_err(poison)?;
            let resolve =
                |source: &RegisteredDirectoryRoot| -> anyhow::Result<Arc<RegisteredRoot>> {
                    anyhow::ensure!(
                        source.role == ObjectRole::Root,
                        "observer accepts root roles only"
                    );
                    let root = live
                        .get(&source.receipt.id)
                        .ok_or_else(|| anyhow::anyhow!("observer source is Unwitnessed"))?;
                    anyhow::ensure!(
                        root.receipt == source.receipt
                            && source.receipt.transfer.executor_boot == self.boot,
                        "observer immutable registration proof mismatch"
                    );
                    Ok(root.clone())
                };
            [resolve(&request.ancestor)?, resolve(&request.subject)?]
        };
        // Retained old versions may be observed for reconciliation. This is deliberately not the
        // launch/Interactive current-binding gate; kernel must revalidate its current fence itself.
        let owner = self.clone();
        let query = request.clone();
        let worker = tokio::task::spawn_blocking(move || owner.observe_held_pair(roots, &query));
        let outcome = match tokio::time::timeout(
            OBSERVATION_DEADLINE + std::time::Duration::from_secs(2),
            worker,
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => DirectoryRelationOutcome::Unknown(DirectoryRelationUnknown::HelperExited),
            Err(_) => DirectoryRelationOutcome::Unknown(DirectoryRelationUnknown::Timeout),
        };
        Ok(ExecutionReply::DirectoryRelation(DirectoryRelationReport {
            request,
            outcome,
        }))
    }

    fn observe_held_pair(
        &self,
        roots: [Arc<RegisteredRoot>; 2],
        request: &DirectoryRelationRequest,
    ) -> DirectoryRelationOutcome {
        match current_directory_pair(roots[0].objects.root(), roots[1].objects.root()) {
            Err(failure) => pair_failure(failure),
            Ok(facts) => {
                let expected = [&request.ancestor.receipt, &request.subject.receipt];
                if expected.iter().zip([&facts.ancestor, &facts.subject]).any(
                    |(receipt, actual)| {
                        receipt.transfer.shape.mount_namespace != facts.mount_namespace
                            || receipt.transfer.shape.objects.first() != Some(actual)
                    },
                ) {
                    DirectoryRelationOutcome::Unknown(DirectoryRelationUnknown::ObjectsChanged)
                } else {
                    let Ok(helper) = self.helper_image.try_clone() else {
                        return DirectoryRelationOutcome::Unknown(
                            DirectoryRelationUnknown::HelperIo,
                        );
                    };
                    let (Ok(ancestor), Ok(subject)) = (
                        roots[0].objects.root().try_clone(),
                        roots[1].objects.root().try_clone(),
                    ) else {
                        return DirectoryRelationOutcome::Unknown(
                            DirectoryRelationUnknown::ObjectsChanged,
                        );
                    };
                    let directories = [ancestor, subject];
                    let reference = |source: &RegisteredDirectoryRoot| ProbeRoot {
                        registration: source.receipt.id.clone(),
                        incarnation: source.receipt.transfer.incarnation.clone(),
                        role: source.role.clone(),
                    };
                    let context = ProbeContext {
                        executor_boot: self.boot.clone(),
                        nonce: request.nonce.clone(),
                        ancestor: reference(&request.ancestor),
                        subject: reference(&request.subject),
                        facts,
                        mechanism: self.mechanism.clone(),
                    };
                    observe(helper, directories, context)
                }
            }
        }
    }
}
