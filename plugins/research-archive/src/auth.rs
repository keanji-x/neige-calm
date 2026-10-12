//! Token storage and the `OAuth` client.
//!
//! The archive keeps its Longbridge token in `<data_dir>/oauth/`, never under
//! `HOME` (where the SDK's default `FileTokenStorage` writes,
//! `rust/crates/oauth/src/token.rs:16-23` at `longbridge/openapi@v5.2.0`). The
//! file shape is the SDK's own token file, the serde JSON of `OAuthToken`
//! with the fields `client_id`, `access_token`, `refresh_token` and
//! `expires_at` (`token.rs:26-36`), so a file the SDK wrote can be imported as
//! is.
//!
//! Token strings only ever live in [`Secret`], whose `Debug` and `Display`
//! print `<redacted>`. The SDK's own `StoredToken` derives `Debug` and prints
//! both tokens (`rust/crates/oauth/src/storage.rs:9-19`), so it is converted at
//! the trait boundary and never formatted.
//!
//! The SDK runs the browser flow from `OAuthBuilder::build` when storage has
//! no token, or when a token expiring within five minutes fails to refresh
//! (`rust/crates/oauth/src/builder.rs:105-153`). [`build_oauth`], which a
//! capture uses, stops that flow at its first step and returns
//! [`OAuthUnavailable::BrowserFlow`]; only [`login`] lets it run.

use std::{
    fmt, fs, io,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

use longbridge::oauth::{OAuth, OAuthBuilder, OAuthError, OAuthResult, StoredToken, TokenStorage};
use serde::{Deserialize, Serialize};

use crate::{files, sdk_error};

/// A credential string. Formatting never shows it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The credential itself, for handing to the SDK only.
    fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the credential starts with `prefix`, without exposing it.
    pub fn has_prefix(&self, prefix: &str) -> bool {
        self.0.starts_with(prefix)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// An OAuth client id. It names the token file, so it is a plain file name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientId(String);

impl ClientId {
    pub fn parse(value: &str) -> Result<Self, TokenFileError> {
        let valid = !value.is_empty()
            && value.len() <= 128
            && !value.starts_with('.')
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(TokenFileError::InvalidClientId)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for ClientId {
    type Err = TokenFileError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// The local port of the browser login's redirect URI,
/// `http://localhost:<port>/callback` (`rust/crates/oauth/src/client.rs:125-144`).
/// It must be the port registered for the OAuth client (60355 for the client
/// the archive takes over from invest), so it is never 0. Only the browser
/// login binds it: oauth2 4.4.2's refresh request sends no `redirect_uri`
/// (`oauth2-4.4.2/src/lib.rs:1438-1457`: `endpoint_request` gets `None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallbackPort(std::num::NonZeroU16);

impl CallbackPort {
    pub fn new(port: u16) -> Option<Self> {
        std::num::NonZeroU16::new(port).map(Self)
    }

    pub fn get(self) -> u16 {
        self.0.get()
    }
}

/// One token file: the SDK's `OAuthToken` shape.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenFile {
    pub client_id: String,
    pub access_token: Secret,
    pub refresh_token: Option<Secret>,
    /// Unix seconds at which the access token expires.
    pub expires_at: u64,
}

impl From<&StoredToken> for TokenFile {
    fn from(token: &StoredToken) -> Self {
        Self {
            client_id: token.client_id.clone(),
            access_token: Secret::new(token.access_token.clone()),
            refresh_token: token.refresh_token.clone().map(Secret::new),
            expires_at: token.expires_at,
        }
    }
}

impl From<TokenFile> for StoredToken {
    fn from(token: TokenFile) -> Self {
        Self {
            client_id: token.client_id,
            access_token: token.access_token.expose().to_owned(),
            refresh_token: token.refresh_token.map(|t| t.expose().to_owned()),
            expires_at: token.expires_at,
        }
    }
}

/// A token file problem. No variant carries file content: a JSON parse error
/// keeps only its category and position, since serde's message can quote a
/// value.
#[derive(Debug, thiserror::Error)]
pub enum TokenFileError {
    #[error("invalid OAuth client id: use 1-128 of [A-Za-z0-9._-], not starting with '.'")]
    InvalidClientId,
    #[error("token file {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("token file {path} is not a token: {category} error at line {line}, column {column}")]
    Parse {
        path: PathBuf,
        category: &'static str,
        line: usize,
        column: usize,
    },
    #[error("token file {path} belongs to another OAuth client id")]
    ClientIdMismatch { path: PathBuf },
    #[error("token storage {dir} is read-only")]
    ReadOnly { dir: PathBuf },
    #[error("token file {path}: `{field}` is {problem}")]
    Invalid {
        path: PathBuf,
        field: &'static str,
        problem: &'static str,
    },
    #[error(
        "the stored token expires later ({stored}) than the imported one ({imported}); pass --force to replace it"
    )]
    ReplacesLaterToken {
        stored: TokenSummary,
        imported: TokenSummary,
    },
    #[error(
        "the imported token is unusable: it has no refresh token and expires within {SDK_REFRESH_MARGIN_SECS} s ({imported}); pass --force to store it anyway"
    )]
    Unusable { imported: TokenSummary },
}

/// Read and parse one token file in the SDK's format.
///
/// The path must be a regular file of bounded size ([`files::read_regular`]),
/// and the token must pass [`TokenFile::validate`].
pub fn parse_token_file(path: &Path) -> Result<TokenFile, TokenFileError> {
    let bytes = files::read_regular(path).map_err(|source| TokenFileError::Io {
        path: path.to_owned(),
        source,
    })?;
    let token: TokenFile = serde_json::from_slice(&bytes).map_err(|e| TokenFileError::Parse {
        path: path.to_owned(),
        category: match e.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "data",
            serde_json::error::Category::Eof => "eof",
        },
        line: e.line(),
        column: e.column(),
    })?;
    token
        .validate()
        .map_err(|(field, problem)| TokenFileError::Invalid {
            path: path.to_owned(),
            field,
            problem,
        })?;
    Ok(token)
}

/// Whether a credential can be sent at all: non-empty, and only visible
/// ASCII (`!` to `~`). That is stricter than an HTTP header value, which the
/// SDK builds from the access token (`Bearer <token>`,
/// `httpclient/src/request.rs:348-349`) and refuses locally otherwise.
fn credential_problem(value: &Secret) -> Option<&'static str> {
    if value.0.is_empty() {
        Some("empty")
    } else if !value.0.bytes().all(|b| b.is_ascii_graphic()) {
        Some("not visible ASCII")
    } else {
        None
    }
}

impl TokenFile {
    /// The one check of a token's contents, for every read and write: the
    /// access token, and the refresh token when present, must be sendable
    /// ([`credential_problem`]). `Err` names the field and the problem,
    /// never the value.
    pub fn validate(&self) -> Result<(), (&'static str, &'static str)> {
        if let Some(problem) = credential_problem(&self.access_token) {
            return Err(("access_token", problem));
        }
        if let Some(problem) = self.refresh_token.as_ref().and_then(credential_problem) {
            return Err(("refresh_token", problem));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    ReadWrite,
    ReadOnly,
}

/// Token storage in `<dir>/<client_id>.json`, mode 0600, written atomically.
#[derive(Clone, Debug)]
pub struct DataDirTokenStorage {
    dir: PathBuf,
    access: Access,
}

impl DataDirTokenStorage {
    /// Writable storage. Creates `dir` with mode 0700 when it is missing.
    ///
    /// An existing `dir` is tightened to 0700; its parents (the data dir)
    /// keep their mode.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, TokenFileError> {
        let dir = dir.into();
        let io_err = |source| TokenFileError::Io {
            path: dir.clone(),
            source,
        };
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(io_err)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(io_err)?;
        Ok(Self {
            dir,
            access: Access::ReadWrite,
        })
    }

    /// Storage whose `save` always refuses and never touches the disk. For
    /// reading only (`auth status`, tests). Never give it to an `OAuth` that
    /// can refresh: the SDK redeems the refresh token at the server before it
    /// calls `save` (`oauth/src/client.rs:108-111`), so the refusal comes
    /// after the old refresh token may already be spent.
    pub fn read_only(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            access: Access::ReadOnly,
        }
    }

    pub fn path_for(&self, client_id: &ClientId) -> PathBuf {
        self.dir.join(format!("{}.json", client_id.as_str()))
    }

    /// Load the token for `client_id`: `Ok(None)` when no file exists, an
    /// error when one exists but is unreadable, malformed, or for another
    /// client id. Our own code calls this; the SDK calls [`TokenStorage::load`].
    pub fn load_checked(&self, client_id: &ClientId) -> Result<Option<TokenFile>, TokenFileError> {
        let path = self.path_for(client_id);
        let token = match parse_token_file(&path) {
            Ok(token) => token,
            Err(TokenFileError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(err) => return Err(err),
        };
        if token.client_id != client_id.as_str() {
            return Err(TokenFileError::ClientIdMismatch { path });
        }
        Ok(Some(token))
    }

    /// Persist `token` atomically through [`files::replace`]: a 0600 temp
    /// file with a random name in the same directory, fsync, rename over the
    /// target, fsync the directory.
    pub fn save_checked(&self, token: &TokenFile) -> Result<(), TokenFileError> {
        self.save_with_temp_names(token, std::iter::repeat_with(files::random_suffix))
    }

    /// [`save_checked`](Self::save_checked) with the temp-name suffixes
    /// drawn from `suffixes`, so a test can make one collide.
    ///
    /// A name that exists already is skipped, never touched
    /// ([`files::replace_with_temp_names`]).
    fn save_with_temp_names(
        &self,
        token: &TokenFile,
        suffixes: impl IntoIterator<Item = String>,
    ) -> Result<(), TokenFileError> {
        if self.access == Access::ReadOnly {
            return Err(TokenFileError::ReadOnly {
                dir: self.dir.clone(),
            });
        }
        let client_id = ClientId::parse(&token.client_id)?;
        let target = self.path_for(&client_id);
        token
            .validate()
            .map_err(|(field, problem)| TokenFileError::Invalid {
                path: target.clone(),
                field,
                problem,
            })?;
        let bytes = serde_json::to_vec_pretty(token).expect("a token file always serializes");
        files::replace_with_temp_names(&target, &bytes, suffixes)
            .map_err(|files::ReplaceError { path, source }| TokenFileError::Io { path, source })
    }
}

impl TokenStorage for DataDirTokenStorage {
    fn load(&self, client_id: &str) -> Option<StoredToken> {
        let loaded = ClientId::parse(client_id).and_then(|id| self.load_checked(&id));
        match loaded {
            Ok(token) => token.map(Into::into),
            Err(err) => {
                // The SDK's trait has no error channel: a `None` here sends it
                // to the browser flow, which `build_oauth` turns into
                // `AUTH_UNAVAILABLE`. Callers that need the cause use
                // `load_checked` first.
                tracing::error!(error = %err, "stored OAuth token is unusable");
                None
            }
        }
    }

    fn save(&self, token: &StoredToken) -> OAuthResult<()> {
        self.save_checked(&TokenFile::from(token)).map_err(|err| {
            tracing::error!(error = %err, "failed to persist the OAuth token");
            match err {
                TokenFileError::Io { path, source } => OAuthError::TokenFileWrite { path, source },
                other => OAuthError::Other(other.to_string()),
            }
        })
    }
}

/// Why no `OAuth` client could be built. It carries only a fixed variant
/// name ([`crate::sdk_error`]), never SDK error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OAuthUnavailable {
    /// The build (a token refresh, or the browser flow's wait) did not
    /// finish within its deadline.
    #[error("timed out")]
    TimedOut,
    /// The SDK wanted the browser flow: no stored token, or an expiring one
    /// whose refresh failed. Needs `auth import` or `auth login`.
    #[error("browser authorization required")]
    BrowserFlow,
    #[error("{0}")]
    Sdk(&'static str),
}

/// Build the shared `OAuth` client from `storage`, for a capture, within
/// `deadline`.
///
/// A valid stored token is loaded without network. An expiring one is
/// refreshed and saved through `storage`; oauth2's HTTP client has no
/// timeout of its own, so `deadline` bounds the refresh. When the SDK falls
/// back to the browser flow, the build stops there and returns
/// [`OAuthUnavailable::BrowserFlow`]: a capture must never wait on a human.
/// The SDK binds its callback listener and then calls `open_url`
/// (`client.rs:125-153`); `open_url` only signals, and the build future,
/// listener included, is dropped before this returns. The listener asks for
/// an ephemeral port (0), so the registered port stays free for
/// `auth login`.
pub async fn build_oauth(
    client_id: &ClientId,
    storage: DataDirTokenStorage,
    deadline: std::time::Duration,
) -> Result<OAuth, OAuthUnavailable> {
    // `notify_one` keeps a permit when nobody waits yet, so the signal is
    // never lost.
    let browser = std::sync::Arc::new(tokio::sync::Notify::new());
    let signal = browser.clone();
    let build = OAuthBuilder::new(client_id.as_str())
        .callback_port(0)
        .token_storage(storage)
        .build(move |_authorization_url| {
            tracing::error!(
                "OAuth browser authorization required; run `auth import` or `auth login`"
            );
            signal.notify_one();
        });
    tokio::select! {
        built = tokio::time::timeout(deadline, build) => match built {
            Ok(Ok(oauth)) => Ok(oauth),
            Ok(Err(err)) => Err(OAuthUnavailable::Sdk(sdk_error::oauth_kind(&err))),
            Err(_elapsed) => Err(OAuthUnavailable::TimedOut),
        },
        () = browser.notified() => Err(OAuthUnavailable::BrowserFlow),
    }
}

/// Token storage for [`login`]: it reports no stored token, so the SDK always
/// runs the browser flow, and it saves through to the data dir.
struct FreshLogin(DataDirTokenStorage);

impl TokenStorage for FreshLogin {
    fn load(&self, _client_id: &str) -> Option<StoredToken> {
        None
    }

    fn save(&self, token: &StoredToken) -> OAuthResult<()> {
        self.0.save(token)
    }
}

/// How long `auth login` may take in all: the SDK's five-minute wait for the
/// browser callback (`oauth/src/callback.rs:12`) plus a margin for the code
/// exchange, which oauth2's HTTP client does not bound
/// (`oauth2-4.4.2/src/reqwest.rs:96-108` builds it without a timeout).
pub const LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300 + 60);

/// Run the SDK's browser login for `client_id` on the registered
/// `callback_port`, and save the new token through `storage`. `show_url`
/// receives the authorization URL for the operator. This is the only path
/// that runs the browser flow. The whole login, the code exchange included,
/// ends by `deadline` ([`LOGIN_TIMEOUT`] for the CLI); a login cut off there
/// has saved nothing, since the SDK saves only after the exchange.
pub async fn login(
    client_id: &ClientId,
    storage: DataDirTokenStorage,
    callback_port: CallbackPort,
    show_url: impl Fn(&str),
    deadline: std::time::Duration,
) -> Result<(), OAuthUnavailable> {
    let build = OAuthBuilder::new(client_id.as_str())
        .callback_port(callback_port.get())
        .token_storage(FreshLogin(storage))
        .build(show_url);
    match tokio::time::timeout(deadline, build).await {
        Ok(Ok(_oauth)) => Ok(()),
        Ok(Err(err)) => Err(OAuthUnavailable::Sdk(sdk_error::oauth_kind(&err))),
        Err(_elapsed) => Err(OAuthUnavailable::TimedOut),
    }
}

/// What a stored token is, without the token itself: what `auth import`,
/// `auth login` and `auth status` print.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenSummary {
    /// Unix seconds at which the access token expires.
    pub expires_at: u64,
    pub has_refresh_token: bool,
}

/// The SDK refreshes a token that expires within this many seconds
/// (`oauth/src/token.rs:59-66`, `expires_soon`).
pub const SDK_REFRESH_MARGIN_SECS: u64 = 300;

impl TokenSummary {
    /// Whether the SDK could use the token at `now` (Unix seconds) without a
    /// browser login: it has a refresh token, or it is not yet within the
    /// SDK's refresh margin.
    pub fn usable_at(&self, now: u64) -> bool {
        self.has_refresh_token || self.expires_at.saturating_sub(now) >= SDK_REFRESH_MARGIN_SECS
    }
}

impl From<&TokenFile> for TokenSummary {
    fn from(token: &TokenFile) -> Self {
        Self {
            expires_at: token.expires_at,
            has_refresh_token: token.refresh_token.is_some(),
        }
    }
}

impl std::fmt::Display for TokenSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let expires = i64::try_from(self.expires_at)
            .ok()
            .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
            .map_or_else(|| "out of range".to_owned(), |t| t.to_rfc3339());
        write!(
            f,
            "expires_at={} ({expires}) refresh_token={}",
            self.expires_at,
            if self.has_refresh_token {
                "present"
            } else {
                "absent"
            }
        )
    }
}

/// Unix seconds now, from the wall clock the SDK compares expiry with.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Import an SDK-written token file into `storage`. The file must belong to
/// `client_id`, and is only read.
///
/// Unless `force`, the import refuses to replace a stored token that
/// expires later than the imported one, and refuses a token that is
/// unusable at `now` (see [`TokenSummary::usable_at`]).
pub fn import_token(
    from: &Path,
    client_id: &ClientId,
    storage: &DataDirTokenStorage,
    force: bool,
    now: u64,
) -> Result<TokenSummary, TokenFileError> {
    import_with_temp_names(
        from,
        client_id,
        storage,
        force,
        now,
        std::iter::repeat_with(files::random_suffix),
    )
}

fn import_with_temp_names(
    from: &Path,
    client_id: &ClientId,
    storage: &DataDirTokenStorage,
    force: bool,
    now: u64,
    suffixes: impl IntoIterator<Item = String>,
) -> Result<TokenSummary, TokenFileError> {
    let token = parse_token_file(from)?;
    if token.client_id != client_id.as_str() {
        return Err(TokenFileError::ClientIdMismatch {
            path: from.to_owned(),
        });
    }
    let imported = TokenSummary::from(&token);
    if !force {
        if !imported.usable_at(now) {
            return Err(TokenFileError::Unusable { imported });
        }
        if let Some(stored) = storage.load_checked(client_id)? {
            let stored = TokenSummary::from(&stored);
            if stored.expires_at > imported.expires_at {
                return Err(TokenFileError::ReplacesLaterToken { stored, imported });
            }
        }
    }
    storage.save_with_temp_names(&token, suffixes)?;
    Ok(imported)
}

#[cfg(test)]
mod tests;
