//! The same persistence assertions run against the fake and production SQLite port.
use super::*;

#[async_trait]
pub trait StoreHarness: Store {
    async fn begin(&self) -> Self::Tx<'_>;
    async fn commit(&self, tx: Self::Tx<'_>);
    async fn rollback(&self, tx: Self::Tx<'_>);
}
#[async_trait]
impl StoreHarness for MemoryStore {
    async fn begin(&self) -> Self::Tx<'_> {
        MemoryTx::default()
    }
    async fn commit(&self, tx: Self::Tx<'_>) {
        MemoryStore::commit(self, tx);
    }
    async fn rollback(&self, _: Self::Tx<'_>) {}
}

pub async fn store<S: StoreHarness>(store: &S) {
    let intent = intent("/".into());
    let id = &intent.metadata.id;
    assert!(store.load(id).await.unwrap().is_none());
    let mut tx = store.begin().await;
    store.insert(&mut tx, &intent).await.unwrap();
    store.rollback(tx).await;
    assert!(store.load(id).await.unwrap().is_none());
    let mut tx = store.begin().await;
    store.insert(&mut tx, &intent).await.unwrap();
    store.request(&mut tx, id, Desired::Stop).await.unwrap();
    store.request(&mut tx, id, Desired::Run).await.unwrap();
    store.commit(tx).await;
    let Some(Stored::Active {
        intent: metadata,
        secrets,
        desired,
        settlement,
    }) = store.load(id).await.unwrap()
    else {
        panic!("admitted execution must be Active")
    };
    assert_eq!(metadata.id, *id);
    assert_eq!(metadata.alias, intent.metadata.alias);
    assert_eq!(metadata.track, intent.metadata.track);
    assert_eq!(metadata.owner, intent.metadata.owner);
    assert_eq!(metadata.ticket_id, intent.metadata.ticket_id);
    assert_eq!(metadata.specification, intent.metadata.specification);
    assert_eq!(metadata.program, intent.metadata.program);
    assert_eq!(metadata.source.root, intent.metadata.source.root);
    assert_eq!(metadata.source.cwd, intent.metadata.source.cwd);
    assert_eq!(
        metadata.source.registration,
        intent.metadata.source.registration
    );
    assert_eq!(metadata.source.nonce, intent.metadata.source.nonce);
    assert_eq!(metadata.source.directory, intent.metadata.source.directory);
    assert_eq!(
        metadata.source.incarnation,
        intent.metadata.source.incarnation
    );
    assert_eq!(metadata.source.authority, intent.metadata.source.authority);
    assert!(
        secrets
            .tokens
            .observe
            .matches(&intent.secrets.tokens.observe)
    );
    assert!(
        secrets
            .tokens
            .interactive
            .matches(&intent.secrets.tokens.interactive)
    );
    assert!(secrets.ticket.matches(&intent.secrets.ticket));
    assert!(
        secrets
            .tokens
            .lifecycle
            .matches(&intent.secrets.tokens.lifecycle)
    );
    assert_eq!(desired, Desired::Stop);
    assert!(settlement.is_none());
    assert!(store.pending().await.unwrap().contains(id));
    assert!(store.finish(id).await.is_err());
    store.spawned(id).await.unwrap();
    store.spawned(id).await.unwrap();
    let evidence = Settlement::Journal(ExecutionState::Revoked);
    store.settle(id, &evidence).await.unwrap();
    store
        .settle(
            id,
            &Settlement::Journal(ExecutionState::NoChild {
                reason: "late evidence".into(),
            }),
        )
        .await
        .unwrap();
    let Some(Stored::Active {
        secrets,
        settlement,
        ..
    }) = store.load(id).await.unwrap()
    else {
        panic!("settled Stop retains capabilities")
    };
    assert!(secrets.ticket.matches(&intent.secrets.ticket));
    assert_eq!(settlement, Some(evidence.clone()));
    assert!(!store.pending().await.unwrap().contains(id));
    let mut tx = store.begin().await;
    store.request(&mut tx, id, Desired::Retire).await.unwrap();
    store.rollback(tx).await;
    assert!(!store.pending().await.unwrap().contains(id));
    let mut tx = store.begin().await;
    store.request(&mut tx, id, Desired::Retire).await.unwrap();
    store.request(&mut tx, id, Desired::Stop).await.unwrap();
    store.commit(tx).await;
    assert!(store.pending().await.unwrap().contains(id));
    store.finish(id).await.unwrap();
    store.finish(id).await.unwrap();
    let Some(Stored::Finished {
        intent: metadata,
        settlement,
    }) = store.load(id).await.unwrap()
    else {
        panic!("cleared capabilities must load as Finished")
    };
    assert_eq!(metadata.id, *id);
    assert_eq!(settlement, evidence);
    assert!(!store.pending().await.unwrap().contains(id));
}

#[cfg(test)]
mod port_contract {
    #[tokio::test]
    async fn store() {
        super::store(&super::MemoryStore::default()).await;
    }
}
