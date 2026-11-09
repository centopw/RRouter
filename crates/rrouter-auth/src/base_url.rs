//! Which base URL an account actually talks to, and when a spec refuses to answer.
//!
//! A provider can ship a *placeholder* host (Azure OpenAI's `RESOURCE.openai.azure.com`)
//! because the real host names the tenant. Its manifest sets `base_url_requires_override`,
//! and every connection must then supply the resource URL. The rule is enforced on the
//! request path as well as at save time: a row can predate the check or be edited around
//! the API, and a DNS failure is a far worse diagnostic than the message here.

use crate::credentials::Credentials;
use crate::manifest::{host_of, is_placeholder_host};
use crate::registry::ProviderSpec;
use rrouter_core::Error;

/// The base URL this account will use, or an error naming what the operator must fix.
///
/// Returns the same `Unauthorized` class the refresher uses for an unusable account, so
/// the dispatcher skips this connection and tries the next one: a second Azure account
/// with a real resource URL still serves the request.
pub fn effective_base_url<'a>(
    spec: &'a ProviderSpec,
    override_url: Option<&'a str>,
) -> Result<&'a str, Error> {
    match validate_base_url_override(spec, override_url) {
        Ok(()) => Ok(override_url.unwrap_or(spec.base_url)),
        // A dispatch-time failure must be retryable per *account*: another connection to
        // the same provider may carry a real resource URL.
        Err(error) => Err(Error::Unauthorized(error.to_string())),
    }
}

/// Whether this connection may be saved with `override_url`.
///
/// A placeholder host is refused whether it came from the spec or from an operator who
/// copied `RESOURCE` into the form: the first is "this provider needs a resource URL", the
/// second is the same mistake spelled out. The error is a `400`: the save request is what
/// is wrong, and a connection form can show the message verbatim.
pub fn validate_base_url_override(
    spec: &ProviderSpec,
    override_url: Option<&str>,
) -> Result<(), Error> {
    let url = override_url.unwrap_or(spec.base_url);
    if !is_placeholder_host(url) {
        return Ok(());
    }
    let host = host_of(url).unwrap_or(url);
    let message = if spec.base_url_requires_override && override_url.is_none() {
        format!(
            "provider {} requires a connection base URL: set it to the endpoint this account \
             serves (for example https://<resource>.openai.azure.com); the default host {host} \
             is a placeholder",
            spec.id
        )
    } else {
        format!(
            "provider {} cannot use the placeholder host {host}: set the connection base URL to \
             the endpoint this account serves",
            spec.id
        )
    };
    Err(Error::BadRequest(message))
}

/// The base URL for one outbound call, checked.
///
/// `build_headers` calls this so no request is ever dispatched to a placeholder host.
pub fn checked_base_url<'a>(spec: &'a ProviderSpec, c: &'a Credentials) -> Result<&'a str, Error> {
    effective_base_url(spec, c.base_url_override.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry;

    fn azure() -> &'static ProviderSpec {
        registry::get("azure").expect("azure manifest")
    }

    #[test]
    fn a_provider_with_a_real_host_needs_no_override() {
        let spec = registry::get("mistral").expect("mistral");
        assert_eq!(effective_base_url(spec, None).unwrap(), spec.base_url);
        assert_eq!(
            effective_base_url(spec, Some("https://gateway.internal/v1/chat/completions")).unwrap(),
            "https://gateway.internal/v1/chat/completions"
        );
        // A normal provider may not point at a placeholder either.
        assert!(effective_base_url(spec, Some("https://RESOURCE.openai.azure.com/v1")).is_err());
    }

    #[test]
    fn a_placeholder_provider_refuses_to_dispatch_without_a_resource_url() {
        let spec = azure();
        assert!(spec.base_url_requires_override);
        let err = effective_base_url(spec, None).expect_err("must refuse the placeholder host");
        assert_eq!(err.status(), 401, "an unusable account, not a bad request");
        let message = err.to_string();
        assert!(
            message.contains("requires a connection base URL"),
            "{message}"
        );
        assert!(message.contains("RESOURCE.openai.azure.com"), "{message}");
    }

    #[test]
    fn a_resource_url_makes_the_connection_usable() {
        let spec = azure();
        assert_eq!(
            effective_base_url(spec, Some("https://contoso.openai.azure.com")).unwrap(),
            "https://contoso.openai.azure.com"
        );
        assert!(validate_base_url_override(spec, Some("https://contoso.openai.azure.com")).is_ok());
    }

    #[test]
    fn an_override_that_still_says_resource_is_refused() {
        let spec = azure();
        let err = validate_base_url_override(spec, Some("https://RESOURCE.openai.azure.com"))
            .expect_err("copying the placeholder is the same mistake");
        assert_eq!(err.status(), 400, "a save request is what is wrong");
        assert!(err.to_string().contains("placeholder host"), "{err}");
    }

    #[test]
    fn the_checked_url_comes_from_the_connection() {
        let spec = azure();
        let creds = Credentials {
            base_url_override: Some("https://contoso.openai.azure.com".into()),
            ..Credentials::default()
        };
        assert_eq!(
            checked_base_url(spec, &creds).unwrap(),
            "https://contoso.openai.azure.com"
        );
        assert!(checked_base_url(spec, &Credentials::default()).is_err());
    }
}
