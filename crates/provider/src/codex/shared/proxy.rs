//! Proxy settings for a child an agent provider spawns with `env_clear()`: the Codex daemon and a
//! Claude Planner both resolve theirs here.

/// The settings value when one is set, else the first non-empty `env_keys` entry from `lookup`.
pub fn effective_proxy_env_from(
    settings_value: Option<&str>,
    env_keys: &[&str],
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some(v) = settings_value {
        return Some(v.to_string());
    }
    env_keys
        .iter()
        .find_map(|key| lookup(key).filter(|v| !v.is_empty()))
}

/// Settings-first, parent-env-fallback proxy resolution as explicit (UPPER, lower, value)
/// pairs; with `env_clear()` the fallback must be SET explicitly.
pub fn resolved_proxy_env_pairs(
    http_settings: Option<&str>,
    https_settings: Option<&str>,
    lookup: impl Fn(&str) -> Option<String> + Copy,
) -> Vec<(&'static str, &'static str, String)> {
    let mut pairs = Vec::new();
    if let Some(v) = effective_proxy_env_from(http_settings, &["HTTP_PROXY", "http_proxy"], lookup)
        .filter(|v| !v.is_empty())
    {
        pairs.push(("HTTP_PROXY", "http_proxy", v));
    }
    if let Some(v) =
        effective_proxy_env_from(https_settings, &["HTTPS_PROXY", "https_proxy"], lookup)
            .filter(|v| !v.is_empty())
    {
        pairs.push(("HTTPS_PROXY", "https_proxy", v));
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With `env_clear()`, a parent-env proxy must be RESOLVED and set explicitly when settings
    /// are absent; settings still win over the parent env.
    #[test]
    fn resolved_proxy_pairs_settings_first_then_explicit_parent_env_fallback() {
        let pairs = resolved_proxy_env_pairs(Some("http://settings-proxy:3128"), None, |key| {
            (key == "HTTP_PROXY").then(|| "http://env-proxy:8080".to_string())
        });
        assert_eq!(
            pairs,
            vec![(
                "HTTP_PROXY",
                "http_proxy",
                "http://settings-proxy:3128".to_string()
            )]
        );

        let pairs = resolved_proxy_env_pairs(None, None, |key| {
            (key == "HTTPS_PROXY").then(|| "http://env-secure:3129".to_string())
        });
        assert_eq!(
            pairs,
            vec![(
                "HTTPS_PROXY",
                "https_proxy",
                "http://env-secure:3129".to_string()
            )]
        );

        assert!(
            resolved_proxy_env_pairs(None, None, |_| None).is_empty(),
            "no settings + no parent env => no proxy keys in the child env"
        );
    }
}
