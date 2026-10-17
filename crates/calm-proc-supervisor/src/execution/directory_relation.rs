use super::{directory_probe::*, *};
use std::{fs::File, os::fd::OwnedFd};
use workspace::execution::objects::*;

async fn observation_result(
    worker: tokio::task::JoinHandle<anyhow::Result<DirectoryRelationOutcome>>,
) -> anyhow::Result<DirectoryRelationOutcome> {
    match tokio::time::timeout(
        OBSERVATION_DEADLINE + std::time::Duration::from_secs(2),
        worker,
    )
    .await
    {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => Ok(DirectoryRelationOutcome::Unknown(
            DirectoryRelationUnknown::HelperExited,
        )),
        Err(_) => Ok(DirectoryRelationOutcome::Unknown(
            DirectoryRelationUnknown::Timeout,
        )),
    }
}

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
        // Old continuously retained versions are reconciliation evidence, not execution admission.
        let owner = self.clone();
        let query = request.clone();
        let worker =
            tokio::task::spawn_blocking(move || Ok(owner.observe_held_pair(roots, &query)));
        let outcome = observation_result(worker).await?;
        Ok(ExecutionReply::DirectoryRelation(Box::new(
            DirectoryRelationReport { request, outcome },
        )))
    }

    pub(super) async fn observe_transferred_directories(
        self: &Arc<Self>,
        request: TransferredDirectoryRelationRequest,
        descriptors: Vec<OwnedFd>,
    ) -> anyhow::Result<ExecutionReply> {
        anyhow::ensure!(
            request.executor_boot == self.boot,
            "transferred observer boot mismatch"
        );
        anyhow::ensure!(
            descriptors.len() == 2,
            "transferred observer requires exactly ancestor and subject descriptors"
        );
        let directories: [File; 2] = descriptors
            .into_iter()
            .map(File::from)
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| anyhow::anyhow!("transferred observer descriptor count changed"))?;
        let owner = self.clone();
        let query = request.clone();
        let worker = tokio::task::spawn_blocking(move || {
            owner.observe_transferred_pair(directories, &query)
        });
        let outcome = observation_result(worker).await?;
        Ok(ExecutionReply::TransferredDirectoryRelation(Box::new(
            TransferredDirectoryRelationReport { request, outcome },
        )))
    }

    fn observe_transferred_pair(
        &self,
        directories: [File; 2],
        request: &TransferredDirectoryRelationRequest,
    ) -> anyhow::Result<DirectoryRelationOutcome> {
        let qualified = qualify_directory_pair(&directories[0], &directories[1])?;
        anyhow::ensure!(
            qualified.facts() == &request.facts,
            "transferred observer actual descriptor facts/roles mismatch"
        );
        let outcome = self.observe_pair(
            [directories[0].try_clone()?, directories[1].try_clone()?],
            [
                ProbeOrigin::Transferred {
                    role: TransferredDirectoryRole::Ancestor,
                },
                ProbeOrigin::Transferred {
                    role: TransferredDirectoryRole::Subject,
                },
            ],
            request.facts.clone(),
            request.nonce.clone(),
        );
        qualified.revalidate()?;
        Ok(outcome)
    }

    fn observe_held_pair(
        &self,
        roots: [Arc<RegisteredRoot>; 2],
        request: &DirectoryRelationRequest,
    ) -> DirectoryRelationOutcome {
        let facts = match current_directory_pair(roots[0].objects.root(), roots[1].objects.root()) {
            Ok(facts) => facts,
            Err(failure) => return pair_failure(failure),
        };
        let expected = [&request.ancestor.receipt, &request.subject.receipt];
        if expected
            .iter()
            .zip([&facts.ancestor, &facts.subject])
            .any(|(receipt, actual)| {
                receipt.transfer.shape.mount_namespace != facts.mount_namespace
                    || receipt.transfer.shape.objects.first() != Some(actual)
            })
        {
            return DirectoryRelationOutcome::Unknown(DirectoryRelationUnknown::ObjectsChanged);
        }
        let (Ok(ancestor), Ok(subject)) = (
            roots[0].objects.root().try_clone(),
            roots[1].objects.root().try_clone(),
        ) else {
            return DirectoryRelationOutcome::Unknown(DirectoryRelationUnknown::ObjectsChanged);
        };
        let origin = |source: &RegisteredDirectoryRoot| ProbeOrigin::Registered {
            registration: source.receipt.id.clone(),
            incarnation: source.receipt.transfer.incarnation.clone(),
        };
        self.observe_pair(
            [ancestor, subject],
            [origin(&request.ancestor), origin(&request.subject)],
            facts,
            request.nonce.clone(),
        )
    }

    // The one helper mechanism for both retained registrations and one-shot qualified FD pairs.
    fn observe_pair(
        &self,
        directories: [File; 2],
        origins: [ProbeOrigin; 2],
        facts: DirectoryPairFacts,
        nonce: TransferNonce,
    ) -> DirectoryRelationOutcome {
        let Ok(helper) = self.helper_image.try_clone() else {
            return DirectoryRelationOutcome::Unknown(DirectoryRelationUnknown::HelperIo);
        };
        let [ancestor, subject] = origins;
        observe(
            helper,
            directories,
            ProbeContext {
                executor_boot: self.boot.clone(),
                nonce,
                ancestor: ProbeRoot {
                    origin: ancestor,
                    role: ObjectRole::Root,
                },
                subject: ProbeRoot {
                    origin: subject,
                    role: ObjectRole::Root,
                },
                facts,
                mechanism: self.mechanism.clone(),
            },
        )
    }
}
