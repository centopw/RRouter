//! Process-wide OAuth endpoint overrides for operators and local integration tests.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

use rrouter_core::{Error, Result};

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

/// OAuth client id for `provider`: `GOOGLE_ANTIGRAVITY_CLIENT_ID` for the built-in
/// Antigravity client, else `RROUTER_OAUTH_CLIENT_ID_<PROVIDER>`, else the registered
/// default. Read per call so an operator rotation applies without a restart.
pub fn client_id_for(provider: &str, default: &'static str) -> Cow<'static, str> {
    client_env(provider, "CLIENT_ID")
        .map(Cow::Owned)
        .unwrap_or(Cow::Borrowed(default))
}

/// OAuth client secret for `provider`, with the same precedence as [`client_id_for`].
///
/// The value is credential material: callers put it in a form body and never log it.
pub fn client_secret_for(
    provider: &str,
    default: Option<&'static str>,
) -> Option<Cow<'static, str>> {
    client_env(provider, "CLIENT_SECRET")
        .map(Cow::Owned)
        .or_else(|| default.map(Cow::Borrowed))
}

fn client_env(provider: &str, kind: &str) -> Option<String> {
    let normalized = normalize_provider(provider);
    let alias = match (normalized.as_str(), kind) {
        ("GOOGLE_ANTIGRAVITY", "CLIENT_ID") => Some("GOOGLE_ANTIGRAVITY_CLIENT_ID"),
        ("GOOGLE_ANTIGRAVITY", "CLIENT_SECRET") => Some("GOOGLE_ANTIGRAVITY_CLIENT_SECRET"),
        _ => None,
    };
    alias
        .and_then(read_nonempty)
        .or_else(|| read_nonempty(&format!("RROUTER_OAUTH_{kind}_{normalized}")))
}

/// Client id/secret for one OAuth exchange or refresh: the environment override wins,
/// otherwise the registered (possibly built-in public) client is used.
pub fn oauth_client(
    provider: &str,
    default_id: &'static str,
    default_secret: Option<&'static str>,
) -> Result<(Cow<'static, str>, Option<Cow<'static, str>>)> {
    if provider == crate::antigravity::PROVIDER_ID {
        // A client id and secret are a pair: mixing an override with the built-in
        // client produces a confusing `invalid_client`, so require both or neither.
        match (
            client_env(provider, "CLIENT_ID"),
            client_env(provider, "CLIENT_SECRET"),
        ) {
            (None, None) => {}
            (Some(id), Some(secret)) => {
                return Ok((Cow::Owned(id), Some(Cow::Owned(secret))));
            }
            _ => {
                return Err(Error::BadRequest(
                    "set both GOOGLE_ANTIGRAVITY_CLIENT_ID and \
                     GOOGLE_ANTIGRAVITY_CLIENT_SECRET, or neither to use the built-in client"
                        .into(),
                ));
            }
        }
    }
    Ok((
        client_id_for(provider, default_id),
        client_secret_for(provider, default_secret),
    ))
}

fn read_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
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

    #[test]
    fn endpoint_validation_requires_https_except_localhost_opt_in() {
        let _guard = crate::TEST_ENV
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

    #[test]
    fn client_credentials_prefer_the_provider_specific_env() {
        let _guard = crate::TEST_ENV
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let id = "GOOGLE_ANTIGRAVITY_CLIENT_ID";
        let secret = "GOOGLE_ANTIGRAVITY_CLIENT_SECRET";
        let generic_id = "RROUTER_OAUTH_CLIENT_ID_GOOGLE_ANTIGRAVITY";
        let generic_secret = "RROUTER_OAUTH_CLIENT_SECRET_GOOGLE_ANTIGRAVITY";

        for name in [id, secret, generic_id, generic_secret] {
            std::env::remove_var(name);
        }

        assert_eq!(
            client_id_for("google-antigravity", "static-id"),
            Cow::Borrowed("static-id")
        );
        assert_eq!(
            client_secret_for("google-antigravity", Some("static-secret")),
            Some(Cow::Borrowed("static-secret"))
        );

        std::env::set_var(generic_id, "generic-id");
        std::env::set_var(generic_secret, "generic-secret");
        assert_eq!(
            client_id_for("google-antigravity", "static-id").as_ref(),
            "generic-id"
        );
        assert_eq!(
            client_secret_for("google-antigravity", Some("static-secret")).as_deref(),
            Some("generic-secret")
        );

        std::env::set_var(id, "specific-id");
        std::env::set_var(secret, "specific-secret");
        assert_eq!(
            client_id_for("google-antigravity", "static-id").as_ref(),
            "specific-id"
        );
        assert_eq!(
            client_secret_for("google-antigravity", Some("static-secret")).as_deref(),
            Some("specific-secret")
        );

        std::env::remove_var(id);
        std::env::remove_var(secret);
        std::env::remove_var(generic_id);
        std::env::remove_var(generic_secret);
    }

    #[test]
    fn google_antigravity_requires_both_env_credentials() {
        let _guard = crate::TEST_ENV
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let names = [
            "GOOGLE_ANTIGRAVITY_CLIENT_ID",
            "GOOGLE_ANTIGRAVITY_CLIENT_SECRET",
            "RROUTER_OAUTH_CLIENT_ID_GOOGLE_ANTIGRAVITY",
            "RROUTER_OAUTH_CLIENT_SECRET_GOOGLE_ANTIGRAVITY",
        ];
        for name in names {
            std::env::remove_var(name);
        }

        let (id, secret) = oauth_client(
            "google-antigravity",
            crate::antigravity::DEFAULT_CLIENT_ID,
            Some(crate::antigravity::DEFAULT_CLIENT_SECRET),
        )
        .expect("built-in client");
        assert_eq!(id.as_ref(), crate::antigravity::DEFAULT_CLIENT_ID);
        assert_eq!(
            secret.as_deref(),
            Some(crate::antigravity::DEFAULT_CLIENT_SECRET)
        );

        // One variable is not enough.
        std::env::set_var("GOOGLE_ANTIGRAVITY_CLIENT_ID", "env-id");
        assert!(
            oauth_client(
                "google-antigravity",
                crate::antigravity::DEFAULT_CLIENT_ID,
                Some(crate::antigravity::DEFAULT_CLIENT_SECRET)
            )
            .is_err(),
            "secret is still missing"
        );

        std::env::set_var("GOOGLE_ANTIGRAVITY_CLIENT_SECRET", "env-secret");
        let (id, secret) = oauth_client(
            "google-antigravity",
            crate::antigravity::DEFAULT_CLIENT_ID,
            Some(crate::antigravity::DEFAULT_CLIENT_SECRET),
        )
        .expect("env");
        assert_eq!(id.as_ref(), "env-id");
        assert_eq!(secret.as_deref(), Some("env-secret"));

        for name in names {
            std::env::remove_var(name);
        }
    }
}
