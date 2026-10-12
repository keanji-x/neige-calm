//! The calendar refresh's error mapping: only credential failures are
//! `auth`, and only a variant name is kept.

use research_archive::calendar::fetch::{RefreshError, refresh_error};

#[test]
fn only_credential_failures_are_auth_and_only_names_are_kept() {
    use longbridge::httpclient::HttpClientError as E;
    let cases = [
        (
            longbridge::Error::OAuth("CANARY expired".into()),
            true,
            "oauth",
        ),
        (
            longbridge::Error::HttpClient(E::InvalidAccessToken),
            true,
            "http:invalid_access_token",
        ),
        (
            longbridge::Error::HttpClient(E::OAuth("CANARY".into())),
            true,
            "http:oauth",
        ),
        (
            longbridge::Error::HttpClient(E::RequestTimeout),
            false,
            "http:request_timeout",
        ),
        (
            longbridge::Error::UnknownCommand(1),
            false,
            "unknown_command",
        ),
    ];
    for (err, auth, kind) in cases {
        let mapped = refresh_error(err);
        let text = mapped.to_string();
        assert!(!text.contains("CANARY"), "{text}");
        match mapped {
            RefreshError::Auth(k) => assert!(auth && k == kind, "{k}"),
            RefreshError::Failed(k) => assert!(!auth && k == kind, "{k}"),
        }
    }
}
