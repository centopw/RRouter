//! Endpoint resolution for provider-native capabilities.
//!
//! A capability endpoint (rerank, files, batches …) is a full URL reviewed in the
//! provider's manifest or the core table. A connection may redirect it to its own origin —
//! a self-hosted gateway, a mock, a regional deployment — but the reviewed **path** is
//! kept, because the path is what identifies the capability, and a manifest-declared query
//! parameter is applied on top (a parameter the connection's own URL already carries wins).

use crate::query::with_query;
use crate::registry::ProviderSpec;

/// The reviewed endpoint, moved to the connection's own origin when it overrides one, with
/// the provider's static query parameters applied.
pub fn capability_endpoint(
    native: &str,
    override_url: Option<&str>,
    spec: &ProviderSpec,
) -> String {
    let base = match override_url {
        None => native.to_string(),
        Some(base) => match (reqwest::Url::parse(native), reqwest::Url::parse(base)) {
            (Ok(native), Ok(mut out)) => {
                out.set_path(native.path());
                out.to_string()
            }
            // An override that is not a URL is the operator's value; send it rather than
            // silently keeping the reviewed host.
            _ => base.to_string(),
        },
    };
    with_query(&base, spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;

    fn openai() -> &'static ProviderSpec {
        registry::get("openai").expect("openai")
    }

    #[test]
    fn without_an_override_the_reviewed_endpoint_stands() {
        let spec = openai();
        assert_eq!(
            capability_endpoint("https://api.openai.com/v1/files", None, spec),
            "https://api.openai.com/v1/files"
        );
    }

    #[test]
    fn an_override_moves_the_origin_and_keeps_the_reviewed_path() {
        let spec = openai();
        assert_eq!(
            capability_endpoint(
                "https://api.openai.com/v1/files",
                Some("http://127.0.0.1:8080"),
                spec
            ),
            "http://127.0.0.1:8080/v1/files"
        );
    }

    #[test]
    fn the_overrides_own_query_survives_and_a_manifest_parameter_fills_gaps() {
        let spec = ProviderSpec {
            query: &[("api-version", "2024-10-21")],
            ..*openai()
        };
        let url = capability_endpoint(
            "https://api.openai.com/v1/files",
            Some("https://tenant.example/v1/files?trace=1"),
            &spec,
        );
        assert_eq!(
            url,
            "https://tenant.example/v1/files?trace=1&api-version=2024-10-21"
        );

        // A parameter the connection already set wins over the manifest's default.
        let url = capability_endpoint(
            "https://api.openai.com/v1/files",
            Some("https://tenant.example?api-version=preview"),
            &spec,
        );
        assert_eq!(url, "https://tenant.example/v1/files?api-version=preview");
    }
}
