//! Outbound URL shaping: the static query parameters a transport declares.
//!
//! Some OpenAI-compatible transports require a query parameter on every request — Azure
//! OpenAI's dated surface needs `api-version`, and a tenant may need a specific version
//! or `preview`. A manifest declares the reviewed default; a connection's
//! `base_url_override` is the deployment's own statement about its endpoint, so it wins
//! and the declared value only fills the gap.
//!
//! The proxy applies this to the URL it computed from the provider's native path (see
//! `rrouter_gateway::proxy::endpoint`): no credential ever rides in a URL, and the values
//! are validated tokens, so nothing here needs percent-encoding.

use crate::registry::ProviderSpec;

/// Append `spec.query` to `url`, keeping any parameter the URL already carries.
///
/// Order is preserved: existing parameters first, then the declared ones the URL lacks.
/// A fragment is carried across untouched.
pub fn with_query(url: &str, spec: &ProviderSpec) -> String {
    if spec.query.is_empty() {
        return url.to_string();
    }
    let (rest, fragment) = match url.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (url, None),
    };
    let (base, existing) = match rest.split_once('?') {
        Some((base, query)) => (base, query),
        None => (rest, ""),
    };
    let mut params: Vec<String> = existing
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(str::to_owned)
        .collect();
    for (name, value) in spec.query {
        let present = params.iter().any(|pair| {
            pair.split_once('=')
                .map(|(key, _)| key)
                .unwrap_or(pair.as_str())
                .eq_ignore_ascii_case(name)
        });
        if !present {
            params.push(format!("{name}={value}"));
        }
    }
    let mut out = base.to_string();
    if !params.is_empty() {
        out.push('?');
        out.push_str(&params.join("&"));
    }
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;

    /// A spec that declares the parameter Azure's dated surface requires.
    fn dated_azure() -> ProviderSpec {
        ProviderSpec {
            query: &[("api-version", "2024-10-21")],
            ..*registry::get("mistral").expect("mistral")
        }
    }

    #[test]
    fn a_url_without_a_query_gains_the_declared_parameters() {
        let spec = dated_azure();
        assert_eq!(
            with_query(
                "https://api.example.test/openai/deployments/chat/completions",
                &spec
            ),
            "https://api.example.test/openai/deployments/chat/completions?api-version=2024-10-21"
        );
    }

    #[test]
    fn an_operator_parameter_wins_and_is_not_duplicated() {
        let spec = dated_azure();
        let out = with_query(
            "https://tenant.openai.azure.com/openai/deployments/chat/completions?api-version=preview",
            &spec,
        );
        assert!(
            out.ends_with("completions?api-version=preview"),
            "the connection's own value stands: {out}"
        );
        assert_eq!(out.matches("api-version").count(), 1, "{out}");
    }

    #[test]
    fn other_parameters_are_preserved_before_the_declared_one() {
        let spec = dated_azure();
        let out = with_query("https://host/v1/chat/completions?trace=1", &spec);
        assert_eq!(
            out,
            "https://host/v1/chat/completions?trace=1&api-version=2024-10-21"
        );
    }

    #[test]
    fn a_provider_without_query_parameters_is_untouched() {
        let spec = *registry::get("mistral").expect("mistral");
        for url in [
            "https://api.mistral.ai/v1/chat/completions",
            "https://api.mistral.ai/v1/chat/completions?x=1",
            "https://api.mistral.ai/v1/chat/completions?",
        ] {
            assert_eq!(with_query(url, &spec), url);
        }
    }

    #[test]
    fn a_fragment_is_carried_across_untouched() {
        let spec = dated_azure();
        assert_eq!(
            with_query("https://host/v1/chat#section", &spec),
            "https://host/v1/chat?api-version=2024-10-21#section"
        );
    }

    #[test]
    fn an_empty_query_after_a_trailing_question_mark_collapses() {
        let spec = dated_azure();
        assert_eq!(
            with_query("https://host/v1/chat?", &spec),
            "https://host/v1/chat?api-version=2024-10-21"
        );
    }
}
