//! Shared Planner wrapper and per-execution identity; providers partition their environment.
use super::*;
use std::{ffi::OsString, path::Path};
#[allow(clippy::too_many_arguments)]
pub(crate) async fn intent(
    consumer: &str,
    session: &str,
    track: &str,
    cwd: &Path,
    id: &ExecutionId,
    dir: &Path,
    binary: &Path,
    args: Vec<OsString>,
    env: Vec<(String, OsString)>,
    prefix: &str,
    payload: Vec<u8>,
) -> Result<(ExecutionIntent, Vec<u8>)> {
    let text = |value: OsString| {
        value
            .into_string()
            .map_err(|_| CalmError::BadRequest("provider arguments require UTF-8".into()))
    };
    let root = tokio::fs::canonicalize(cwd).await?;
    let mut wrapper_args = vec![
        "-c".into(),
        stderr::wrapper(prefix),
        "neige-provider".into(),
        text(dir.as_os_str().to_owned())?,
        text(root.as_os_str().to_owned())?,
        text(binary.as_os_str().to_owned())?,
    ];
    wrapper_args.extend(args.into_iter().map(text).collect::<Result<Vec<_>>>()?);
    let environment = env
        .into_iter()
        .map(|(key, value)| Ok((key, text(value)?)))
        .collect::<Result<Vec<_>>>()?;
    Ok((
        ExecutionIntent {
            metadata: ExecutionMetadata {
                consumer: consumer.into(),
                id: id.clone(),
                alias: ProcessAlias(super::sweep::alias(session, id.as_str())),
                owner: OperationRef(format!("worker-session:{}", session)),
                track: TrackId(track.to_owned()),
                source: PrivateSource {
                    root,
                    cwd: workspace::identity::WorkspaceCwd::root(),
                    registration: ObjectRegistrationId::fresh()?,
                    nonce: TransferNonce::fresh()?,
                    directory: ExternalDirectoryId::fresh()?,
                    incarnation: ExternalDirectoryIncarnationId::fresh()?,
                    authority: ExecutionAuthority {
                        grant_id: ExecutionGrantId::fresh()?,
                        authorization_binding: AuthorizationBindingId::fresh()?,
                        authorization_version: 1,
                    },
                },
                program: ExecutionSpecification {
                    program: "/bin/sh".into(),
                    args: wrapper_args,
                    environment,
                    io_mode: calm_session::control::IoMode::Pipe,
                    replay_bytes: 4 * 1024 * 1024,
                },
                specification: SpecificationId::fresh()?,
                ticket_id: LaunchTicketId::fresh()?,
            },
            secrets: ExecutionSecrets {
                ticket: CapabilityToken::fresh()?,
                tokens: ExecutionControlTokens::fresh()?,
            },
        },
        payload,
    ))
}
