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
    Ok((
        wrapped_intent(
            LaunchOwner {
                consumer: owner.consumer().into(),
                alias: ProcessAlias(owner.alias(session, id.as_str())),
                owner: OperationRef(format!("worker-session:{session}")),
                scope: ExecutionOwnership::Track(TrackId(track.to_owned())),
            },
            cwd,
            id,
            dir,
            binary,
            args,
            env,
            prefix,
            ambient,
        )
        .await?,
        payload,
    ))
}

pub(crate) struct LaunchOwner {
    pub consumer: ExecutionConsumer,
    pub alias: ProcessAlias,
    pub owner: OperationRef,
    pub scope: ExecutionOwnership,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn wrapped_intent(
    owner: LaunchOwner,
    cwd: &Path,
    id: &ExecutionId,
    dir: &Path,
    binary: &Path,
    args: Vec<OsString>,
    env: Vec<(String, OsString)>,
    prefix: &str,
    ambient: calm_session::control::execution::AmbientEnvironment,
) -> Result<ExecutionIntent> {
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
    Ok(ExecutionIntent {
        metadata: ExecutionMetadata {
            consumer: owner.consumer,
            id: id.clone(),
            alias: owner.alias,
            owner: owner.owner,
            scope: owner.scope,
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
    })
}

/// Public bootstrap inputs and an ephemeral Held release. Project environment
/// values belong entirely in the payload, never the durable public environment.
pub(crate) struct Release {
    pub public: Vec<(String, OsString)>,
    /// Names the payload's keys for the wrapper; carries no values.
    pub read_prefix: String,
    pub payload: Vec<u8>,
}
impl Release {
    /// Install a complete private environment after the owner-supplied bootstrap.
    pub(crate) fn with_private(
        env: Vec<(String, OsString)>,
        private: std::collections::BTreeMap<OsString, OsString>,
        token: Option<String>,
        what: &str,
    ) -> Result<Self> {
        Self::with_private_names(env, private, token, what).map(|(release, _)| release)
    }

    /// The variable-name contract may be persisted; its corresponding values may not.
    pub(crate) fn with_private_names(
        env: Vec<(String, OsString)>,
        private: std::collections::BTreeMap<OsString, OsString>,
        token: Option<String>,
        what: &str,
    ) -> Result<(Self, Vec<String>)> {
        use crate::operation::stdin_payload::{KeySource, PayloadKeys};
        use std::collections::{BTreeMap, BTreeSet};
        let public = Self::split(env, None, what)?.public;
        let mut private = private
            .into_iter()
            .map(|(key, value)| {
                let key = key.into_string().map_err(|_| {
                    CalmError::BadRequest(
                        "private environment requires UTF-8 variable names".into(),
                    )
                })?;
                Ok((key, value))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        if let Some(token) = token {
            private.insert("NEIGE_MCP_TOKEN".into(), token.into());
        }
        // The private map is complete. Remove bootstrap values absent from it
        // instead of silently restoring variables the project explicitly unset.
        let present = private
            .keys()
            .map(String::as_str)
            .chain(public.iter().map(|(key, _)| key.as_str()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let keys = PayloadKeys::new(&present, KeySource::Kernel)?.with_multiline();
        let payload = keys.render(|key| private.get(key).cloned()).map_err(|_| {
            CalmError::BadRequest(format!("invalid {what} private environment payload"))
        })?;
        let names = present.iter().map(|key| (*key).to_owned()).collect();
        let read_prefix = keys.read_prefix();
        Ok((
            Self {
                public,
                read_prefix,
                payload,
            },
            names,
        ))
    }

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

#[cfg(test)]
mod tests;
