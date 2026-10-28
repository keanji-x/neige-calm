//! Shared provider wrapper and per-execution identity; providers partition their environment
//! and declare whether the supervisor merges its ambient allowlist at spawn.
use super::*;
use std::{ffi::OsString, path::Path};
#[allow(clippy::too_many_arguments)]
pub(crate) async fn intent(
    owner: &dyn ProviderExecutionOwner,
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
    ambient: calm_session::control::execution::AmbientEnvironment,
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
                consumer: owner.consumer().into(),
                id: id.clone(),
                alias: ProcessAlias(owner.alias(session, id.as_str())),
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
                    ambient_environment: ambient,
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

/// One launch's environment split at the release boundary: the MCP token and every secret proxy
/// value ride only in the Held stdin payload; everything else is public and durable.
pub(crate) struct Release {
    pub public: Vec<(String, OsString)>,
    /// Names the payload's keys for the wrapper; carries no values.
    pub read_prefix: String,
    pub payload: Vec<u8>,
}
impl Release {
    pub(crate) fn split(
        env: Vec<(String, OsString)>,
        token: Option<String>,
        what: &str,
    ) -> Result<Self> {
        use crate::execution_environment::{PROXY_KEYS, proxy_value_is_secret};
        use crate::operation::stdin_payload::{KeySource, PayloadKeys};
        let mut secrets = std::collections::HashMap::new();
        if let Some(token) = token {
            secrets.insert("NEIGE_MCP_TOKEN".to_string(), token.into());
        }
        // Command::envs resolves duplicate names with the last value winning.
        // Resolve that final environment before classifying either side of the payload.
        let public = env
            .into_iter()
            .filter(|(key, _)| key != "NEIGE_MCP_TOKEN")
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_iter()
            .filter_map(|(key, value)| {
                if PROXY_KEYS.contains(&key.as_str()) && proxy_value_is_secret(&key, &value) {
                    secrets.insert(key, value);
                    None
                } else {
                    Some((key, value))
                }
            })
            .collect::<Vec<_>>();
        // Only classified secret keys belong in the read prefix: non-secret proxy
        // values must retain their environment values rather than be unset.
        let present = PROXY_KEYS
            .iter()
            .copied()
            .chain(["NEIGE_MCP_TOKEN"])
            .filter(|key| secrets.contains_key(*key))
            .collect::<Vec<_>>();
        let keys = PayloadKeys::new(&present, KeySource::Kernel)?;
        let payload = keys
            .render(|key| secrets.get(key).cloned())
            .map_err(|_| CalmError::BadRequest(format!("invalid {what} release payload")))?;
        Ok(Self {
            public,
            read_prefix: keys.read_prefix(),
            payload,
        })
    }
}
