//! Final Planner proxy values are classified before constructing durable launch metadata.
pub(crate) use calm_session::control::execution::proxy::{PROXY_KEYS, proxy_value_is_secret};

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    #[test]
    fn c4_proxy_value_classification_table() {
        use crate::operation::stdin_payload::{KeySource, PayloadKeys};
        assert!(PayloadKeys::new(PROXY_KEYS, KeySource::Kernel).is_ok());
        let mismatches = [
            ("http://proxy.example:8080", false),
            ("socks5://[::1]:1080", false),
            ("https://proxy.example/path@domain", true),
            ("http://[::1]:8080", false),
            ("HtTp://proxy.example:8080", false),
            ("http://user:password@proxy.example", true),
            ("http:///user:password@proxy.invalid:8080", true),
            (r"http:\\user:password@proxy.invalid:8080", true),
            (r"http://\user:password@proxy.invalid:8080", true),
            ("HtTp:///user:password@proxy.invalid:8080", true),
            ("http://%75ser:%70assword@proxy.example", true),
            ("http://user:password@[::1]:8080", true),
            ("http://user:@proxy.example", true),
            ("http://user@proxy.example", true),
            ("http://@proxy.example", true),
            ("http://:@proxy.example", true),
            ("http:///@proxy.example", true),
            ("mailto:user@example.com", true),
            ("not a URL", true),
            ("", true),
        ]
        .into_iter()
        .map(|(value, secret)| ("HTTP_PROXY", value, secret))
        .chain([
            ("NO_PROXY", "localhost,.example.com,127.0.0.1", false),
            ("no_proxy", "*", false),
            ("NO_PROXY", "", false),
            ("NO_PROXY", "http://user:password@proxy.invalid:8080", true),
            ("no_proxy", "http://user:password@proxy.invalid:8080", true),
            (
                "NO_PROXY",
                "localhost,http://user:password@proxy.invalid:8080,.example.com",
                true,
            ),
            (
                "no_proxy",
                "localhost,http://user:password@proxy.invalid:8080,.example.com",
                true,
            ),
            ("no_proxy", "example.com@", true),
        ])
        .filter(|(key, value, secret)| proxy_value_is_secret(key, OsStr::new(value)) != *secret)
        .collect::<Vec<_>>();
        use std::os::unix::ffi::OsStrExt;
        for key in PROXY_KEYS {
            assert!(proxy_value_is_secret(key, OsStr::from_bytes(b"host\xff")));
        }
        assert!(
            mismatches.is_empty(),
            "misclassified proxies: {mismatches:?}"
        );
    }
}
