//! The one way an SDK error is rendered: as a fixed variant name.
//!
//! SDK, oauth2 and reqwest errors carry text the archive does not control:
//! upstream messages and bodies, and the OAuth callback's `error` query
//! parameter, which anyone who can reach the local callback port can set
//! (`oauth/src/callback.rs:41-47,94-99` at `longbridge/openapi@v5.2.0`).
//! Their `Display` and `Debug` are therefore never formatted into a CLI
//! message, a log field, a stored value or a panic. Callers render the
//! `&'static str` these functions return. Every variant is named, so an SDK
//! upgrade that adds one fails to compile here.

use longbridge::{httpclient::HttpClientError, oauth::OAuthError, wsclient::WsClientError};

/// The snake_case name of an OAuth error variant.
pub fn oauth_kind(err: &OAuthError) -> &'static str {
    match err {
        OAuthError::NoHomeDir => "no_home_dir",
        OAuthError::TokenFileRead { .. } => "token_file_read",
        OAuthError::TokenFileParse { .. } => "token_file_parse",
        OAuthError::CreateDirFailed { .. } => "create_dir_failed",
        OAuthError::SerializeToken { .. } => "serialize_token",
        OAuthError::TokenFileWrite { .. } => "token_file_write",
        OAuthError::NoTokenAvailable => "no_token_available",
        OAuthError::BindCallbackFailed { .. } => "bind_callback_failed",
        OAuthError::LocalAddressFailed => "local_address_failed",
        OAuthError::CsrfTokenMismatch => "csrf_token_mismatch",
        OAuthError::ExchangeCodeFailed { .. } => "exchange_code_failed",
        OAuthError::NoRefreshToken => "no_refresh_token",
        OAuthError::RefreshTokenFailed { .. } => "refresh_token_failed",
        OAuthError::AuthorizationFailed { .. } => "authorization_failed",
        OAuthError::CallbackChannelClosed => "callback_channel_closed",
        OAuthError::AuthorizationTimeout => "authorization_timeout",
        OAuthError::Other(_) => "other",
    }
}

/// The snake_case name of an HTTP client error variant.
pub fn http_kind(err: &HttpClientError) -> &'static str {
    match err {
        HttpClientError::InvalidRequestMethod => "invalid_request_method",
        HttpClientError::InvalidApiKey => "invalid_api_key",
        HttpClientError::InvalidAccessToken => "invalid_access_token",
        HttpClientError::MissingEnvVar { .. } => "missing_env_var",
        HttpClientError::UnexpectedResponse => "unexpected_response",
        HttpClientError::RequestTimeout => "request_timeout",
        HttpClientError::DcRegionRestricted { .. } => "dc_region_restricted",
        HttpClientError::OpenApi { .. } => "openapi",
        HttpClientError::DeserializeResponseBody(_) => "deserialize_response_body",
        HttpClientError::SerializeRequestBody(_) => "serialize_request_body",
        HttpClientError::SerializeQueryString(_) => "serialize_query_string",
        HttpClientError::BadStatus(_) => "bad_status",
        HttpClientError::UnexpectedHttpResponse { .. } => "unexpected_http_response",
        HttpClientError::Http(_) => "http",
        HttpClientError::ConnectionLimitExceeded { .. } => "connection_limit_exceeded",
        HttpClientError::OAuth(_) => "oauth",
        HttpClientError::Sse(_) => "sse",
    }
}

/// The snake_case name of a quote WebSocket client error variant.
pub fn ws_kind(err: &WsClientError) -> &'static str {
    match err {
        WsClientError::UnexpectedResponse => "unexpected_response",
        WsClientError::Decode(_) => "decode",
        WsClientError::ConnectTimeout => "connect_timeout",
        WsClientError::RequestTimeout => "request_timeout",
        WsClientError::ConnectionClosed { .. } => "connection_closed",
        WsClientError::ClientClosed => "client_closed",
        WsClientError::ResponseError { .. } => "response_error",
        WsClientError::Cancelled => "cancelled",
        WsClientError::InvalidUrl(_) => "invalid_url",
        WsClientError::Websocket(_) => "websocket",
    }
}

/// The name of an SDK context error (`QuoteContext`'s): the variant, and for
/// a client error the client's variant, as `http:<kind>` or `ws:<kind>`.
pub fn sdk_kind(err: &longbridge::Error) -> std::borrow::Cow<'static, str> {
    use longbridge::Error as E;
    let fixed = match err {
        E::DecodeProtobuf(_) => "decode_protobuf",
        E::DecodeJSON(_) => "decode_json",
        E::ParseField { .. } => "parse_field",
        E::UnknownCommand(_) => "unknown_command",
        E::InvalidSecuritySymbol { .. } => "invalid_security_symbol",
        E::UnknownMarket { .. } => "unknown_market",
        E::UnknownTradeSession { .. } => "unknown_trade_session",
        E::HttpClient(http) => return format!("http:{}", http_kind(http)).into(),
        E::WsClient(ws) => return format!("ws:{}", ws_kind(ws)).into(),
        E::OAuth(_) => "oauth",
        E::ConversationStreamEnded => "conversation_stream_ended",
    };
    fixed.into()
}
