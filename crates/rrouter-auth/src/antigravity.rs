//! Google Antigravity (Cloud Code Assist) sign-in constants and Cloud-project discovery.
//!
//! The registry row lives in [`crate::registry`]; this module owns the Google identity
//! endpoints and the two calls that turn an access token into the project id every CCA
//! envelope must carry (`credentials.extra.project_id`).
//!
//! Endpoint overrides, in precedence order:
//! - `RROUTER_OAUTH_OVERRIDE_GOOGLE_ANTIGRAVITY_{AUTHORIZE,TOKEN}`: sign-in endpoints,
//!   applied by [`crate::login_flow_for`] and the refresher.
//! - `GOOGLE_ANTIGRAVITY_CLIENT_ID` / `GOOGLE_ANTIGRAVITY_CLIENT_SECRET`: optional
//!   override. The [Antigravity public desktop client][client] is built in, so sign-in
//!   works without registering an OAuth client. `RROUTER_OAUTH_CLIENT_ID_GOOGLE_ANTIGRAVITY`
//!   / `RROUTER_OAUTH_CLIENT_SECRET_GOOGLE_ANTIGRAVITY` are accepted fallbacks.
//! - `RROUTER_ANTIGRAVITY_PROD_HOST` / `RROUTER_ANTIGRAVITY_DAILY_HOST`: API origins,
//!   shared with the CCA transport in `rrouter-translate`.
//!
//! Tokens and the client secret are never logged: this module only puts them in request
//! headers and form bodies, and errors carry a truncated upstream body, never credentials.

use std::time::Duration;

use rrouter_core::{Error, Result};
use serde_json::{json, Value};

use crate::credentials::Credentials;

/// Registry id of the provider this module describes.
pub const PROVIDER_ID: &str = "google-antigravity";

/// Antigravity's public desktop OAuth client. Google treats installed-app secrets as
/// public, the same client 9router and the Antigravity IDE ship with; an operator can
/// still override both values with `GOOGLE_ANTIGRAVITY_CLIENT_ID`/`_SECRET`.
pub const DEFAULT_CLIENT_ID: &str = concat!(
    "1071006060591-",
    "tmhssin2h21lcre235vtolojh4g403ep",
    ".apps.googleusercontent.com"
);
pub const DEFAULT_CLIENT_SECRET: &str = concat!("GOCSPX-", "K58FWR", "486LdLJ1mLB8sXC4z6qDAf");

/// Google's authorization endpoint.
pub const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Google's token endpoint. Both the code exchange and refresh require `client_secret`.
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Callback path. The public desktop client accepts any loopback port, so the flow
/// chooses one dynamically (the same shape 9router uses).
pub const CALLBACK_PATH: &str = "/callback";
/// OpenID userinfo, the account label the dashboard shows.
pub const PROFILE_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo?alt=json";

/// Scopes the Antigravity client requests. `cloud-platform` is what authorizes CCA;
/// `cclog` and `experimentsandconfigs` are first-party Antigravity APIs.
pub const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform \
https://www.googleapis.com/auth/userinfo.email \
https://www.googleapis.com/auth/userinfo.profile \
https://www.googleapis.com/auth/cclog \
https://www.googleapis.com/auth/experimentsandconfigs";

/// `Credentials.extra` key the CCA envelope and the proxy read.
pub const PROJECT_ID_KEY: &str = "project_id";

/// Path of the model catalogue on the production CCA host. Unlike the OpenAI-style
/// providers this is a `POST` that carries the account's Cloud project.
pub const FETCH_MODELS_PATH: &str = "/v1internal:fetchAvailableModels";

/// Project id stored on a credential, trimmed and non-empty.
///
/// The gateway's connection view and CCA transport both read this; a missing value is a
/// configuration error, never an empty project on the wire.
pub fn project_id(credentials: &Credentials) -> Option<&str> {
    credentials
        .extra
        .get(PROJECT_ID_KEY)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|project| !project.is_empty())
}

/// One model a CCA account can call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableModel {
    pub id: String,
    /// `displayName` from the catalogue, when the backend sends one.
    pub display: Option<String>,
}

/// Parse `{"models": {"<id>": {"displayName", "quotaInfo"}}}`.
///
/// Keys are the model ids the envelope must carry; `quotaInfo` is deliberately not
/// persisted (the model registry has no quota column, and account quota is a separate
/// surface). Empty ids are dropped, and serde's map ordering makes the result stable.
pub fn available_models(body: &Value) -> Vec<AvailableModel> {
    let Some(models) = body.get("models").and_then(Value::as_object) else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|(id, meta)| {
            let id = id.trim();
            if id.is_empty() {
                return None;
            }
            let display = meta
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string);
            Some(AvailableModel {
                id: id.to_string(),
                display,
            })
        })
        .collect()
}

/// Metadata every Antigravity code-assist call declares.
fn metadata() -> Value {
    json!({
        "ideType": 9,
        "platform": platform_enum(),
        "pluginType": 2
    })
}

/// Antigravity's `ClientMetadata.Platform` enum: 1 macOS x64, 2 macOS arm64,
/// 3 Linux x64, 4 Linux arm64, 5 Windows. The shared desktop client is identified by
/// these values; string enums are rejected by project discovery.
fn platform_enum() -> i32 {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "x86_64") => 1,
        ("macos", "aarch64") => 2,
        ("linux", "x86_64") => 3,
        ("linux", "aarch64") => 4,
        ("windows", _) => 5,
        _ => 0,
    }
}

/// Daily host for API traffic: the env override wins, else the provider's configured base.
pub fn daily_base(configured: &str) -> String {
    rrouter_translate::google_cca::daily_host(configured)
}

/// Production host used for `loadCodeAssist` and as the CCA fallback.
pub fn prod_base() -> String {
    rrouter_translate::google_cca::prod_host()
}

/// Resolve the account's Cloud project, onboarding when the account has none.
///
/// 1. `POST <prod>/v1internal:loadCodeAssist`; a `cloudaicompanionProject`, `projectId`
///    or `project.id` in the answer ends the search.
/// 2. Otherwise `POST <daily>/v1internal:onboardUser` with the account's default tier and
///    poll at most five times, two seconds apart, until the answer is `done`.
///
/// The returned id is stored as `credentials.extra.project_id`. A missing project is a
/// configuration/entitlement error with a message naming the next action, never a panic.
pub async fn discover_project(
    http: &reqwest::Client,
    access_token: &str,
    prod_base: &str,
    daily_base: &str,
) -> Result<String> {
    discover_with(
        http,
        access_token,
        prod_base,
        daily_base,
        ONBOARD_POLLS,
        ONBOARD_POLL_INTERVAL,
    )
    .await
}

const ONBOARD_POLLS: usize = 5;
const ONBOARD_POLL_INTERVAL: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// [`discover_project`] with a caller-chosen poll budget, so tests need not wait 2 s.
async fn discover_with(
    http: &reqwest::Client,
    access_token: &str,
    prod_base: &str,
    daily_base: &str,
    polls: usize,
    interval: Duration,
) -> Result<String> {
    let load = post_json(
        http,
        &join(prod_base, "/v1internal:loadCodeAssist"),
        access_token,
        &json!({ "metadata": metadata() }),
    )
    .await?;
    if let Some(project) = extract_project(&load) {
        return Ok(project);
    }

    let body = json!({
        "tierId": tier_id(&load),
        "metadata": metadata()
    });
    for attempt in 0..polls.max(1) {
        if attempt > 0 {
            tokio::time::sleep(interval).await;
        }
        let value = post_json(
            http,
            &join(daily_base, "/v1internal:onboardUser"),
            access_token,
            &body,
        )
        .await?;
        if let Some(project) = extract_project(&value) {
            return Ok(project);
        }
        if value.get("done").and_then(Value::as_bool) == Some(true) {
            break;
        }
    }

    Err(Error::BadRequest(format!(
        "{PROVIDER_ID} sign-in found no Cloud project; enable Antigravity/Code Assist for \
         this account at https://antigravity.google and sign in again"
    )))
}

/// Project id from any documented response shape.
///
/// `cloudaicompanionProject` is a bare id on some responses and `{id, projectId}` on
/// others; `projectId` and `project.id` appear in onboarding payloads. The `response`
/// member wraps every `onboardUser` result.
pub fn extract_project(value: &Value) -> Option<String> {
    for key in ["cloudaicompanionProject", "projectId", "project"] {
        if let Some(project) = project_value(&value[key]) {
            return Some(project);
        }
    }
    value.get("response").and_then(extract_project)
}

fn project_value(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => non_empty(s),
        Value::Object(_) => ["id", "projectId", "name"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str).and_then(non_empty)),
        _ => None,
    }
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Tier the onboarding call should use: the response's default allowed tier, else its
/// first named tier, else Google's standard tier.
fn tier_id(load: &Value) -> String {
    let tiers = load.get("allowedTiers").and_then(Value::as_array);
    let chosen = tiers.and_then(|list| {
        list.iter()
            .find(|tier| tier.get("isDefault").and_then(Value::as_bool) == Some(true))
            .or_else(|| list.first())
    });
    chosen
        .and_then(|tier| {
            tier.get("tierId")
                .or_else(|| tier.get("id"))
                .and_then(Value::as_str)
        })
        .and_then(non_empty)
        .unwrap_or_else(|| "standard-tier".to_string())
}

async fn post_json(
    http: &reqwest::Client,
    url: &str,
    access_token: &str,
    body: &Value,
) -> Result<Value> {
    let resp = http
        .post(url)
        .bearer_auth(access_token)
        .header("accept", "application/json")
        .header("user-agent", rrouter_translate::google_cca::USER_AGENT)
        .header(
            "x-goog-api-client",
            rrouter_translate::google_cca::CLIENT_HEADER,
        )
        .header("x-request-source", "local")
        .timeout(REQUEST_TIMEOUT)
        .json(body)
        .send()
        .await
        .map_err(transport)?;
    let status = resp.status();
    let text = resp.text().await.map_err(transport)?;
    if !status.is_success() {
        return Err(Error::Upstream {
            provider: PROVIDER_ID.to_string(),
            status: status.as_u16(),
            body: truncate(&text, 512),
            payload: serde_json::from_str(&text).ok(),
        });
    }
    serde_json::from_str(&text).map_err(|e| Error::Upstream {
        provider: PROVIDER_ID.to_string(),
        status: status.as_u16(),
        body: format!("unparsable project response: {e}"),
        payload: None,
    })
}

fn join(base: &str, path: &str) -> String {
    format!("{}{path}", base.trim_end_matches('/'))
}

fn transport(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout(format!("google-antigravity project request: {e}"))
    } else {
        Error::Transport(format!("google-antigravity project request: {e}"))
    }
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    #[test]
    fn project_extraction_covers_every_documented_shape() {
        assert_eq!(
            extract_project(&json!({"cloudaicompanionProject": "proj-a"})).as_deref(),
            Some("proj-a")
        );
        assert_eq!(
            extract_project(&json!({"cloudaicompanionProject": {"id": "proj-b"}})).as_deref(),
            Some("proj-b")
        );
        assert_eq!(
            extract_project(&json!({"projectId": "proj-c"})).as_deref(),
            Some("proj-c")
        );
        assert_eq!(
            extract_project(&json!({"project": {"id": "proj-d"}})).as_deref(),
            Some("proj-d")
        );
        assert_eq!(
            extract_project(&json!({"response": {"cloudaicompanionProject": "proj-e"}})).as_deref(),
            Some("proj-e")
        );
        assert_eq!(
            extract_project(&json!({"cloudaicompanionProject": "  "})),
            None
        );
        assert_eq!(extract_project(&json!({})), None);
    }

    #[test]
    fn onboarding_uses_the_default_tier_and_falls_back() {
        assert_eq!(
            tier_id(&json!({
                "allowedTiers": [
                    {"id": "free-tier"},
                    {"tierId": "standard-tier", "isDefault": true}
                ]
            })),
            "standard-tier"
        );
        assert_eq!(
            tier_id(&json!({"allowedTiers": [{"id": "free-tier"}]})),
            "free-tier"
        );
        assert_eq!(tier_id(&json!({})), "standard-tier");
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client")
    }

    #[tokio::test]
    async fn load_code_assist_returns_the_project_without_onboarding() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1internal:loadCodeAssist"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer at-antigravity",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(json!({"cloudaicompanionProject": "proj-1"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        // The onboarding path must not be touched when loadCodeAssist already knows.
        wiremock::Mock::given(wiremock::matchers::path("/v1internal:onboardUser"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;

        let project = discover_with(
            &client(),
            "at-antigravity",
            &server.uri(),
            &server.uri(),
            2,
            Duration::from_millis(1),
        )
        .await
        .expect("project");
        assert_eq!(project, "proj-1");
    }

    #[tokio::test]
    async fn onboarding_polls_until_done_and_extracts_the_project() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/v1internal:loadCodeAssist"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "allowedTiers": [{"tierId": "standard-tier", "isDefault": true}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_tier = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let polls_for_handler = polls.clone();
        let tier_for_handler = seen_tier.clone();
        wiremock::Mock::given(wiremock::matchers::path("/v1internal:onboardUser"))
            .respond_with(move |req: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                *tier_for_handler.lock().expect("tier") = body
                    .get("tierId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let n = polls_for_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 {
                    return wiremock::ResponseTemplate::new(200)
                        .set_body_json(json!({"done": false}));
                }
                wiremock::ResponseTemplate::new(200).set_body_json(json!({
                    "done": true,
                    "response": {"cloudaicompanionProject": {"id": "proj-onboarded"}}
                }))
            })
            .expect(2)
            .mount(&server)
            .await;

        let project = discover_with(
            &client(),
            "at-antigravity",
            &server.uri(),
            &server.uri(),
            3,
            Duration::from_millis(1),
        )
        .await
        .expect("project");
        assert_eq!(project, "proj-onboarded");
        assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            seen_tier.lock().expect("tier").as_deref(),
            Some("standard-tier")
        );
    }

    #[tokio::test]
    async fn a_missing_project_is_a_clear_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/v1internal:loadCodeAssist"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::path("/v1internal:onboardUser"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({"done": true})))
            .mount(&server)
            .await;

        let err = discover_with(
            &client(),
            "at-antigravity",
            &server.uri(),
            &server.uri(),
            2,
            Duration::from_millis(1),
        )
        .await
        .expect_err("no project");
        assert!(matches!(err, Error::BadRequest(_)), "got {err:?}");
        assert!(err.to_string().contains("no Cloud project"), "{err}");
    }

    #[tokio::test]
    async fn upstream_rejection_is_reported_without_the_token() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/v1internal:loadCodeAssist"))
            .respond_with(
                wiremock::ResponseTemplate::new(403)
                    .set_body_json(json!({"error": {"message": "denied"}})),
            )
            .mount(&server)
            .await;

        let err = discover_with(
            &client(),
            "at-secret-token",
            &server.uri(),
            &server.uri(),
            1,
            Duration::from_millis(1),
        )
        .await
        .expect_err("denied");
        assert!(
            matches!(err, Error::Upstream { status: 403, .. }),
            "got {err:?}"
        );
        assert!(!err.to_string().contains("at-secret-token"), "{err}");
    }

    #[test]
    fn model_catalogue_parses_ids_displays_and_ignores_quota() {
        let models = available_models(&json!({
            "models": {
                "gemini-3-pro": {
                    "displayName": "Gemini 3 Pro",
                    "quotaInfo": {"remainingFraction": 0.5}
                },
                "gemini-2.5-flash": {"displayName": "  "},
                "claude-sonnet-4-5": {"displayName": "Claude Sonnet 4.5"}
            }
        }));
        assert_eq!(
            models,
            vec![
                AvailableModel {
                    id: "claude-sonnet-4-5".into(),
                    display: Some("Claude Sonnet 4.5".into()),
                },
                AvailableModel {
                    id: "gemini-2.5-flash".into(),
                    display: None,
                },
                AvailableModel {
                    id: "gemini-3-pro".into(),
                    display: Some("Gemini 3 Pro".into()),
                },
            ]
        );
        assert!(available_models(&json!({"models": []})).is_empty());
        assert!(available_models(&json!({})).is_empty());
        assert!(available_models(&json!({"models": {" ": {"displayName": "x"}}})).is_empty());
    }

    #[test]
    fn project_id_is_trimmed_and_never_blank() {
        let mut creds = Credentials::default();
        assert_eq!(project_id(&creds), None);
        creds
            .extra
            .insert(PROJECT_ID_KEY.into(), Value::String("  proj-1  ".into()));
        assert_eq!(project_id(&creds), Some("proj-1"));
        creds
            .extra
            .insert(PROJECT_ID_KEY.into(), Value::String("  ".into()));
        assert_eq!(project_id(&creds), None);
    }

    #[test]
    fn builtin_client_is_the_public_antigravity_client() {
        assert!(DEFAULT_CLIENT_ID.ends_with(".apps.googleusercontent.com"));
        assert!(DEFAULT_CLIENT_SECRET.starts_with("GOCSPX-"));
        assert_eq!(CALLBACK_PATH, "/callback");
        let meta = metadata();
        assert_eq!(meta["ideType"], 9);
        assert_eq!(meta["pluginType"], 2);
        assert!(meta["platform"].as_i64().unwrap_or(-1) >= 0);
    }
}
