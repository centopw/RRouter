//! Process-wide OAuth endpoint overrides for operators and local integration tests.

use std::collections::HashMap;
use std::sync::LazyLock;

/// OAuth endpoint kinds that can be overridden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverrideKind {
    Authorize,
    Token,
    Device,
}

struct Overrides {
    values: HashMap<(String, OverrideKind), String>,
    invalid: Vec<String>,
}

static OVERRIDES: LazyLock<Overrides> = LazyLock::new(load_overrides);

/// Return the configured endpoint for `provider` and `kind`, if it passed validation.
pub fn override_for(provider: &str, kind: OverrideKind) -> Option<&'static str> {
    let key = (normalize_provider(provider), kind);
    OVERRIDES.values.get(&key).map(String::as_str)
}

/// Return the rejected override environment variables, including their values.
pub fn invalid_overrides() -> Vec<String> {
    OVERRIDES.invalid.clone()
}

fn load_overrides() -> Overrides {
    let mut values = HashMap::new();
    let mut invalid = Vec::new();

    for (name, value) in std::env::vars() {
        let Some(suffix) = name.strip_prefix("RROUTER_OAUTH_OVERRIDE_") else {
            continue;
        };
        let (provider, kind) = if let Some(provider) = suffix.strip_suffix("_AUTHORIZE") {
            (provider, OverrideKind::Authorize)
        } else if let Some(provider) = suffix.strip_suffix("_TOKEN") {
            (provider, OverrideKind::Token)
        } else if let Some(provider) = suffix.strip_suffix("_DEVICE") {
            (provider, OverrideKind::Device)
        } else {
            continue;
        };
        if provider.is_empty() || !valid_endpoint(&value) {
            invalid.push(format!("{name}={value}"));
            continue;
        }
        values.insert((normalize_provider(provider), kind), value);
    }

    Overrides { values, invalid }
}

fn normalize_provider(provider: &str) -> String {
    provider.to_ascii_uppercase().replace('-', "_")
}

fn valid_endpoint(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    match url.scheme() {
        "https" => true,
        "http" => {
            matches!(host, "127.0.0.1" | "localhost")
                && std::env::var("RROUTER_ALLOW_INSECURE_OAUTH").as_deref() == Ok("1")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn endpoint_validation_requires_https_except_localhost_opt_in() {
        let _guard = ENV_MUTEX
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        std::env::remove_var("RROUTER_ALLOW_INSECURE_OAUTH");
        assert!(valid_endpoint("https://oauth.example.test/token"));
        assert!(!valid_endpoint("http://oauth.example.test/token"));
        assert!(!valid_endpoint("not a url"));
        std::env::set_var("RROUTER_ALLOW_INSECURE_OAUTH", "1");
        assert!(valid_endpoint("http://127.0.0.1:8080/token"));
        std::env::remove_var("RROUTER_ALLOW_INSECURE_OAUTH");
    }
}
