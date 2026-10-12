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
//! capture uses, turns that flow into `exit(AUTH_UNAVAILABLE)`; only
//! [`login`] lets it run.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use longbridge::oauth::{OAuth, OAuthBuilder, OAuthError, OAuthResult, StoredToken, TokenStorage};
use serde::{Deserialize, Serialize};

use crate::{exit, sdk_error};

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
pub fn parse_token_file(path: &Path) -> Result<TokenFile, TokenFileError> {
    let bytes = fs::read(path).map_err(|source| TokenFileError::Io {
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|e| TokenFileError::Parse {
        path: path.to_owned(),
        category: match e.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "data",
            serde_json::error::Category::Eof => "eof",
        },
        line: e.line(),
        column: e.column(),
    })
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
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, TokenFileError> {
        let dir = dir.into();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|source| TokenFileError::Io {
                path: dir.clone(),
                source,
            })?;
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

    /// Persist `token` atomically: a 0600 temp file with a random name in
    /// the same directory, fsync, rename over the target, fsync the
    /// directory.
    pub fn save_checked(&self, token: &TokenFile) -> Result<(), TokenFileError> {
        self.save_with_temp_names(token, std::iter::repeat_with(random_suffix))
    }

    /// [`save_checked`](Self::save_checked) with the temp-name suffixes
    /// drawn from `suffixes`, so a test can make one collide.
    ///
    /// A name that exists already is skipped, never touched: only a temp
    /// file this call created is renamed or removed.
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
        let io_err = |path: &Path| {
            let path = path.to_owned();
            move |source| TokenFileError::Io { path, source }
        };
        let mut suffixes = suffixes.into_iter().take(TEMP_ATTEMPTS);
        let (temp, mut file) = loop {
            let Some(suffix) = suffixes.next() else {
                return Err(TokenFileError::Io {
                    path: self.dir.clone(),
                    source: io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "no free temp-file name for the token",
                    ),
                });
            };
            let temp = self
                .dir
                .join(format!(".{}.json.tmp-{suffix}", client_id.as_str()));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)
            {
                Ok(file) => break (temp, file),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                // Nothing was created, so there is nothing to remove.
                Err(err) => return Err(io_err(&temp)(err)),
            }
        };
        // From here on `temp` is ours: it is renamed into place or removed.
        let bytes = serde_json::to_vec_pretty(token).expect("a token file always serializes");
        let written = file
            .write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(io_err(&temp))
            .and_then(|()| fs::rename(&temp, &target).map_err(io_err(&target)));
        if let Err(err) = written {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        File::open(&self.dir)
            .and_then(|dir| dir.sync_all())
            .map_err(io_err(&self.dir))
    }
}

/// How many temp-file names one save tries before giving up.
const TEMP_ATTEMPTS: usize = 16;

/// 128 random bits from the OS, as hex: a temp-file suffix no other process
/// can predict.
fn random_suffix() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    hex::encode(bytes)
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
    #[error("{0}")]
    Sdk(&'static str),
}

/// Build the shared `OAuth` client from `storage`, for a capture, within
/// `deadline`.
///
/// A valid stored token is loaded without network. An expiring one is
/// refreshed and saved through `storage`; oauth2's HTTP client has no
/// timeout of its own, so `deadline` bounds the refresh. When the SDK would
/// fall back to the browser flow, the process exits with
/// [`exit::AUTH_UNAVAILABLE`] instead: a capture must never wait on a human.
/// The SDK binds its callback listener before it calls `open_url`
/// (`client.rs:125-153`), so this asks for an ephemeral port (0): the
/// registered port stays free for `auth login`.
pub async fn build_oauth(
    client_id: &ClientId,
    storage: DataDirTokenStorage,
    deadline: std::time::Duration,
) -> Result<OAuth, OAuthUnavailable> {
    let build = OAuthBuilder::new(client_id.as_str())
        .callback_port(0)
        .token_storage(storage)
        .build(|_authorization_url| {
            tracing::error!(
                "OAuth browser authorization required; run `auth import` or `auth login`"
            );
            std::process::exit(exit::AUTH_UNAVAILABLE);
        });
    match tokio::time::timeout(deadline, build).await {
        Ok(Ok(oauth)) => Ok(oauth),
        Ok(Err(err)) => Err(OAuthUnavailable::Sdk(sdk_error::oauth_kind(&err))),
        Err(_elapsed) => Err(OAuthUnavailable::TimedOut),
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

/// Run the SDK's browser login for `client_id` on the registered
/// `callback_port`, and save the new token through `storage`. `show_url`
/// receives the authorization URL for the operator. This is the only path
/// that runs the browser flow. The SDK gives the browser five minutes
/// (`oauth/src/callback.rs:12`).
pub async fn login(
    client_id: &ClientId,
    storage: DataDirTokenStorage,
    callback_port: CallbackPort,
    show_url: impl Fn(&str),
) -> Result<(), OAuthUnavailable> {
    OAuthBuilder::new(client_id.as_str())
        .callback_port(callback_port.get())
        .token_storage(FreshLogin(storage))
        .build(show_url)
        .await
        .map(drop)
        .map_err(|err| OAuthUnavailable::Sdk(sdk_error::oauth_kind(&err)))
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
        std::iter::repeat_with(random_suffix),
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
