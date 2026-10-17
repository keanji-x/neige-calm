//! Single real request preparation never substitutes filesystem preparation or
//! reruns mutable configuration checks on an already accepted key.
use super::*;

#[tokio::test]
async fn original_key_replay_does_not_reprepare_a_changed_workspace_configuration() {
    let mut fx = Fixture::new().await;
    let accepted = fx.accept("original", FIRST).await.unwrap().ack;
    let before = fx.counts().await;
    fx.route.workspace_root = std::path::PathBuf::from("unsupported-relative-configuration");
    assert_replayed(fx.accept("original", FIRST).await.unwrap(), &accepted);
    assert!(matches!(
        fx.accept("fresh-key", FIRST).await,
        Err(CalmError::BadRequest(_))
    ));
    assert_eq!(fx.counts().await, before);
    assert!(!fx.root.exists());
    assert!(!fx.state.shared_codex_appserver.is_running());
}

#[tokio::test]
async fn a_request_requiring_attached_or_template_preparation_cannot_fall_back_to_blank() {
    let fx = Fixture::new().await;
    let mut attached = fx.request(FIRST);
    attached.cwd = Some(fx.root.to_string_lossy().into_owned());
    let mut template = fx.request(FIRST);
    template.template_id = Some("unavailable-template".into());
    let mut recipe = fx.request(FIRST);
    recipe.recipe_id = Some("unavailable-recipe".into());
    for request in [attached, template, recipe] {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "no-fallback".parse().unwrap());
        assert!(matches!(
            accept_dormant_human_track(fx.route.clone(), fx.source.clone(), headers, request).await,
            Err(CalmError::BadRequest(_))
        ));
        assert_eq!(fx.counts().await, (0, 0, 0, 0, 0, 0));
    }
    assert!(!fx.root.exists());
    assert!(!fx.state.shared_codex_appserver.is_running());
}
