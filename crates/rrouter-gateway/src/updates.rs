//! Self-hosted update checks and deployment-aware upgrades.
//!
//! Checking and downloading are safe everywhere: a release is fetched from GitHub, its
//! SHA-256 is verified, and its archive is unpacked into `data_dir/updates/<version>`.
//! Applying is only automatic where replacing the running binary is the deployment's
//! own update mechanism. Containers and Helm releases need their supervisor, so those
//! report the exact command instead of mutating themselves.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use rrouter_core::{Error, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::state::AppState;

const RELEASE_API: &str = "https://api.github.com/repos/centopw/RRouter/releases/latest";
const RELEASES_LATEST: &str = "https://github.com/centopw/RRouter/releases/latest";
const RELEASE_TAG_BASE: &str = "https://github.com/centopw/RRouter/releases/tag";
const RELEASE_DOWNLOAD_BASE: &str = "https://github.com/centopw/RRouter/releases/download";
const RELEASE_PAGE: &str = "https://github.com/centopw/RRouter/releases";
const BREW_FORMULA: &str = "rrouter";
/// Settings key holding the last check so a restart does not immediately re-check.
const STATE_KEY: &str = "update_check";
/// Routine automatic checks are due once a day; manual checks ignore this.
const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;
/// Used when a 403/429 carries no usable `x-ratelimit-reset`.
const RATE_LIMIT_FALLBACK_SECS: i64 = 15 * 60;

/// Where a check sends its requests. Production uses [`Default`]; tests point every URL
/// at a local mock so no request ever reaches GitHub.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub api: String,
    pub releases_latest: String,
    pub release_tag_base: String,
    pub download_base: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            api: RELEASE_API.to_string(),
            releases_latest: RELEASES_LATEST.to_string(),
            release_tag_base: RELEASE_TAG_BASE.to_string(),
            download_base: RELEASE_DOWNLOAD_BASE.to_string(),
        }
    }
}

/// The operator's optional GitHub token. `RROUTER_UPDATE_TOKEN` wins, then `GITHUB_TOKEN`;
/// an empty value is unset. The value is never logged or returned.
fn update_token() -> Option<String> {
    for name in ["RROUTER_UPDATE_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(name) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Check state persisted in the settings blob. Only metadata: never a token or URL query.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Persisted {
    /// The build that wrote this state. A different build (upgrade, downgrade, manual
    /// binary swap) makes the cached release and its validators meaningless.
    current_version: Option<String>,
    checked_at: Option<i64>,
    etag: Option<String>,
    last_modified: Option<String>,
    latest: Option<String>,
    release_url: Option<String>,
    notes: Option<String>,
    published_at: Option<String>,
    rate_limited_until: Option<i64>,
    last_error: Option<String>,
}

impl Persisted {
    /// Drop everything another build learned. Without this, a restart after an upgrade
    /// keeps the old "latest", skips the check as fresh, and a conditional GET with the
    /// old ETag answers 304 and keeps it forever. State written before this field existed
    /// has no version and is dropped once. The rate-limit backoff is per IP, not per
    /// build, so it survives.
    fn trusted(self) -> Self {
        if self.current_version.as_deref() == Some(env!("CARGO_PKG_VERSION")) {
            return self;
        }
        Self {
            rate_limited_until: self.rate_limited_until,
            ..Self::default()
        }
    }
}

/// Whether a URL is a plain `http(s)` URL this build is willing to request. Overrides used
/// by tests are loopback; production constants are https.
fn valid_endpoint(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Constructed asset URL for a tag, used when the API metadata is unavailable.
pub fn download_url(tag: &str, asset: &str, base: &str) -> String {
    format!("{}/{}/{}", base.trim_end_matches('/'), tag, asset)
}

/// `true` when `checked_at` is recent enough that a routine check may skip the network.
fn freshness_ok(now: i64, persisted: &Persisted) -> bool {
    persisted
        .checked_at
        .is_some_and(|at| now.saturating_sub(at) < CHECK_INTERVAL_SECS)
}

/// What a completed check should do for the download step.
enum Discovery {
    /// API metadata (assets included): download from the exact URLs.
    Api(Value),
    /// Redirect/tag discovery: downloads are constructed from the tag.
    Tag(String),
}

fn rate_limit_until(status: u16, headers: &reqwest::header::HeaderMap, now: i64) -> Option<i64> {
    if status != 403 && status != 429 {
        return None;
    }
    let remaining = headers
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    let exhausted = status == 429 || remaining == Some(0);
    if !exhausted {
        return None;
    }
    let reset = headers
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|reset| *reset > now);
    let until = reset.unwrap_or(now + RATE_LIMIT_FALLBACK_SECS);
    // Small deterministic jitter so a NAT full of gateways does not all retry in the same
    // second after the reset.
    Some(until + 5 + until.rem_euclid(25))
}

/// Test hook: clear the in-memory status and re-enable hydration. Integration tests share
/// one process, and each scenario starts from a fresh server state.
#[doc(hidden)]
pub fn reset_for_tests() {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = State::default();
    HYDRATED.store(false, Ordering::SeqCst);
    BUSY.store(false, Ordering::SeqCst);
}

static HYDRATED: AtomicBool = AtomicBool::new(false);

/// Read the persisted check state once per process.
async fn hydrate(state: &std::sync::Arc<AppState>) {
    if HYDRATED.swap(true, Ordering::SeqCst) {
        return;
    }
    let Ok(settings) = crate::settings::load(state).await else {
        return;
    };
    let Some(value) = settings.get(STATE_KEY) else {
        return;
    };
    let Ok(persisted) = serde_json::from_value::<Persisted>(value.clone()) else {
        return;
    };
    let persisted = persisted.trusted();
    let mut status = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    if status.latest.is_none() && persisted.latest.is_some() {
        status.latest = persisted.latest;
        status.release_url = persisted.release_url;
        status.notes = persisted.notes;
        status.published_at = persisted.published_at;
    }
    status.checked_at = status.checked_at.or(persisted.checked_at);
    status.etag = persisted.etag;
    status.last_modified = persisted.last_modified;
    status.rate_limited_until = persisted.rate_limited_until;
    status.last_error = persisted.last_error.clone();
    status.error = persisted.last_error;
}

/// Write the check state to the settings blob. Best effort: a failed write must not fail
/// the check itself, and it is logged without any token or body.
async fn persist(state: &std::sync::Arc<AppState>) {
    let persisted = {
        let status = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        Persisted {
            current_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            checked_at: status.checked_at,
            etag: status.etag.clone(),
            last_modified: status.last_modified.clone(),
            latest: status.latest.clone(),
            release_url: status.release_url.clone(),
            notes: status.notes.clone(),
            published_at: status.published_at.clone(),
            rate_limited_until: status.rate_limited_until,
            last_error: status.last_error.clone(),
        }
    };
    let value = match serde_json::to_value(&persisted) {
        Ok(value) => value,
        Err(e) => {
            tracing::warn!(error = %e, "update: cannot serialize check state");
            return;
        }
    };
    if let Err(e) = crate::settings::merge(state, json!({ STATE_KEY: value })).await {
        tracing::warn!(error = %e, "update: cannot persist check state");
    }
}

static STATUS: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static BUSY: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Default)]
struct State {
    latest: Option<String>,
    release_url: Option<String>,
    notes: Option<String>,
    published_at: Option<String>,
    checked_at: Option<i64>,
    downloaded: Option<PathBuf>,
    phase: Option<String>,
    error: Option<String>,
    /// HTTP validators for conditional requests (`If-None-Match` / `If-Modified-Since`).
    etag: Option<String>,
    last_modified: Option<String>,
    /// When the GitHub API may be called again after a 403/429 exhaustion.
    rate_limited_until: Option<i64>,
    /// Last failure in operator terms; never an upstream body.
    last_error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Deployment {
    /// `binary`, `systemd`, `brew`, `docker`, `kubernetes` or `helm`.
    pub kind: String,
    pub label: String,
    /// Whether this process can finish the upgrade without a supervisor.
    pub automatic: bool,
    /// Whether an archive is staged here. Docker/Helm/Brew replace the whole install.
    pub can_download: bool,
    /// The command the operator runs when `automatic` is false.
    pub command: Option<String>,
    pub detail: String,
}

/// The asset name published by `.github/workflows/release.yml`.
pub fn target_asset() -> Option<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return None,
    };
    Some(format!("rrouter-{os}-{arch}.tar.gz"))
}

/// Detect how this process was deployed. `RROUTER_DEPLOYMENT` wins, which matters for
/// custom images and packaged deployments that cannot be inferred from the filesystem.
pub fn deployment() -> Deployment {
    if let Ok(raw) = std::env::var("RROUTER_DEPLOYMENT") {
        let kind = raw.trim().to_ascii_lowercase();
        if !kind.is_empty() {
            return deployment_for(&kind);
        }
    }
    if std::env::var_os("KUBERNETES_SERVICE_HOST").is_some() {
        let helm = std::env::var("RROUTER_HELM_RELEASE").is_ok();
        return deployment_for(if helm { "helm" } else { "kubernetes" });
    }
    if Path::new("/.dockerenv").exists()
        || std::env::var_os("container").is_some()
        || std::env::var("RROUTER_DEPLOYMENT")
            .map(|v| v == "docker")
            .unwrap_or(false)
    {
        return deployment_for("docker");
    }
    let exe = std::env::current_exe().unwrap_or_default();
    if exe.to_string_lossy().contains("/Cellar/") {
        return deployment_for("brew");
    }
    if std::env::var_os("INVOCATION_ID").is_some() || Path::new("/run/systemd/system").exists() {
        return deployment_for("systemd");
    }
    deployment_for("binary")
}

fn deployment_for(kind: &str) -> Deployment {
    let helm_release = std::env::var("RROUTER_HELM_RELEASE").unwrap_or_else(|_| "rrouter".into());
    let helm_chart =
        std::env::var("RROUTER_HELM_CHART").unwrap_or_else(|_| "rrouter/rrouter".into());
    match kind {
        "brew" => Deployment {
            kind: "brew".into(),
            label: "Homebrew".into(),
            automatic: true,
            can_download: false,
            command: Some(format!("brew upgrade {BREW_FORMULA}")),
            detail: "Upgrades the formula, then restarts the service the same way brew does.".into(),
        },
        "docker" => Deployment {
            kind: "docker".into(),
            label: "Docker".into(),
            automatic: false,
            can_download: false,
            command: Some("docker compose pull && docker compose up -d".into()),
            detail: "A container cannot replace its own image. Pull the new tag and recreate it on the host; the /data volume is preserved.".into(),
        },
        "kubernetes" => Deployment {
            kind: "kubernetes".into(),
            label: "Kubernetes".into(),
            automatic: false,
            can_download: false,
            command: Some("kubectl rollout restart deployment/rrouter".into()),
            detail: "Update the image tag in the manifest, then let the rollout replace the pod. The PVC is preserved.".into(),
        },
        "helm" => Deployment {
            kind: "helm".into(),
            label: "Helm".into(),
            automatic: false,
            can_download: false,
            command: Some(format!(
                "helm upgrade {helm_release} {helm_chart} --reuse-values --set image.tag=<version>"
            )),
            detail: "A pod cannot upgrade its own Helm release. Run the release upgrade on the host so Helm keeps ownership of the resources.".into(),
        },
        "systemd" => Deployment {
            kind: "systemd".into(),
            label: "systemd".into(),
            automatic: true,
            can_download: target_asset().is_some(),
            command: Some("systemctl restart rrouter".into()),
            detail: "The staged binary replaces this executable and execs in place; the unit keeps running under systemd.".into(),
        },
        _ => Deployment {
            kind: "binary".into(),
            label: "Binary".into(),
            automatic: true,
            can_download: target_asset().is_some(),
            command: None,
            detail: "The staged binary replaces this executable and restarts the process in place.".into(),
        },
    }
}

pub async fn snapshot(state: &std::sync::Arc<AppState>) -> Value {
    let options = crate::settings::load(state)
        .await
        .unwrap_or_else(|_| json!({}));
    let status = STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    status_json(&status, &options)
}

fn status_json(status: &State, options: &Value) -> Value {
    let deployment = deployment();
    let current = env!("CARGO_PKG_VERSION");
    // A release older than this build is not "latest" from the operator's point of view:
    // showing it reads as a downgrade. Only a release at or above this build is reported.
    let latest = status
        .latest
        .as_deref()
        .filter(|latest| !newer(latest, current));
    let update_available = latest.is_some_and(|latest| newer(current, latest));
    json!({
        "current_version": current,
        "update_available": update_available,
        "up_to_date": status.latest.is_some() && !update_available,
        "latest_version": latest,
        "release_url": status.release_url.clone().unwrap_or_else(|| RELEASE_PAGE.to_string()),
        "notes": status.notes,
        "published_at": status.published_at,
        "checked_at": status.checked_at,
        "phase": status.phase.clone().unwrap_or_else(|| "idle".into()),
        "error": status.error,
        "last_error": status.last_error,
        "rate_limited_until": status.rate_limited_until,
        "downloaded": status.downloaded.is_some(),
        "deployment": deployment,
        "options": {
            "auto_check": options.get("update_auto_check").and_then(Value::as_bool).unwrap_or(true),
            "auto_download": options.get("update_auto_download").and_then(Value::as_bool).unwrap_or(false),
        },
    })
}

fn set_phase(phase: &str) {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).phase = Some(phase.to_string());
}

/// Compare release tags (`v1.2.3`) and semver-ish versions; pre-releases sort below the
/// same numeric version, and differing segment counts are zero-padded.
pub fn newer(current: &str, candidate: &str) -> bool {
    fn parts(v: &str) -> (Vec<u64>, String) {
        let v = v.trim().trim_start_matches('v');
        let (version, suffix) = v
            .split_once('-')
            .or_else(|| v.split_once('+'))
            .unwrap_or((v, ""));
        let nums = version
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect();
        (nums, suffix.to_ascii_lowercase())
    }
    let (a, a_suffix) = parts(current);
    let (b, b_suffix) = parts(candidate);
    let width = a.len().max(b.len());
    for i in 0..width {
        let left = a.get(i).copied().unwrap_or(0);
        let right = b.get(i).copied().unwrap_or(0);
        if left != right {
            return right > left;
        }
    }
    !a_suffix.is_empty() && b_suffix.is_empty()
}

/// Operator-triggered check (the `/api/update/*` routes). Ignores the daily freshness
/// cache, but never calls the GitHub API before its reset: it falls back to the release
/// redirect and reports when the API will be available again.
pub async fn check(state: &std::sync::Arc<AppState>, download: bool) -> Result<Value> {
    check_with(
        state,
        &Endpoints::default(),
        update_token().as_deref(),
        download,
        true,
    )
    .await
}

/// The scheduler entry point: no network while fresh or rate-limited, and no noisy warn
/// on every restart.
async fn check_automatic(state: &std::sync::Arc<AppState>, download: bool) -> Result<()> {
    check_with(
        state,
        &Endpoints::default(),
        update_token().as_deref(),
        download,
        false,
    )
    .await
    .map(|_| ())
}

enum ApiResult {
    Fresh { release: Value, version: String },
    NotModified,
    RateLimited { until: i64 },
}

async fn check_api(
    state: &AppState,
    endpoints: &Endpoints,
    token: &str,
    persisted: &Persisted,
    now: i64,
) -> Result<ApiResult> {
    let mut request = state
        .http
        .get(&endpoints.api)
        .header("accept", "application/vnd.github+json")
        .header("user-agent", "rrouter")
        .timeout(Duration::from_secs(20));
    // A token raises the per-IP limit; it is sent only as a header and never logged.
    request = request.bearer_auth(token);
    if let Some(etag) = &persisted.etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    if let Some(modified) = &persisted.last_modified {
        request = request.header(reqwest::header::IF_MODIFIED_SINCE, modified);
    }
    let response = request
        .send()
        .await
        .map_err(|e| Error::Transport(format!("update check: {e}")))?;
    let status = response.status();
    let headers = response.headers().clone();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(ApiResult::NotModified);
    }
    if let Some(until) = rate_limit_until(status.as_u16(), &headers, now) {
        return Ok(ApiResult::RateLimited { until });
    }
    if !status.is_success() {
        // The body of a GitHub error can be HTML or a JSON blob; it is useless to an
        // operator and must not become a WARN on every check.
        return Err(Error::Transport(format!(
            "GitHub API returned HTTP {}",
            status.as_u16()
        )));
    }
    let etag = headers
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let last_modified = headers
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let release: Value = response
        .json()
        .await
        .map_err(|e| Error::Transport(format!("update release body: {e}")))?;
    let tag = release
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Transport("release has no tag_name".into()))?
        .to_string();
    {
        let mut state = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        state.etag = etag;
        state.last_modified = last_modified;
    }
    Ok(ApiResult::Fresh {
        version: tag.trim_start_matches('v').to_string(),
        release,
    })
}

/// Version discovery without the REST API: follow `releases/latest` to `/releases/tag/vX`.
async fn discover_via_redirect(state: &AppState, endpoints: &Endpoints) -> Result<String> {
    let response = state
        .http
        .head(&endpoints.releases_latest)
        .header("user-agent", "rrouter")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| Error::Transport(format!("release redirect: {e}")))?;
    if !response.status().is_success() {
        return Err(Error::Transport(format!(
            "release redirect returned HTTP {}",
            response.status().as_u16()
        )));
    }
    let final_url = response.url().clone();
    let path = final_url.path();
    let tag = path
        .rsplit_once("/releases/tag/")
        .map(|(_, tail)| tail.trim_matches('/'))
        .filter(|tag| !tag.is_empty() && tag.len() <= 128)
        .ok_or_else(|| {
            Error::Transport(format!(
                "release redirect did not land on a tag (path {})",
                final_url.path()
            ))
        })?;
    if !tag
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
    {
        return Err(Error::Transport(
            "release tag has an unexpected shape".into(),
        ));
    }
    Ok(tag.to_string())
}

fn rate_limit_message(until: i64, now: i64) -> String {
    let remaining = (until - now).max(0);
    format!(
        "GitHub API rate limit resets at {until} ({remaining}s); checked the release \
         redirect instead"
    )
}

async fn load_persisted(state: &std::sync::Arc<AppState>) -> Persisted {
    let Ok(settings) = crate::settings::load(state).await else {
        return Persisted::default();
    };
    settings
        .get(STATE_KEY)
        .and_then(|value| serde_json::from_value::<Persisted>(value.clone()).ok())
        .unwrap_or_default()
        .trusted()
}

/// Testable core of a check: explicit endpoints, token and mode, so a mock server can
/// stand in for GitHub and no live request leaves the test.
#[doc(hidden)]
pub async fn check_with(
    state: &std::sync::Arc<AppState>,
    endpoints: &Endpoints,
    token: Option<&str>,
    download: bool,
    manual: bool,
) -> Result<Value> {
    if !valid_endpoint(&endpoints.api)
        || !valid_endpoint(&endpoints.releases_latest)
        || !valid_endpoint(&endpoints.release_tag_base)
        || !valid_endpoint(&endpoints.download_base)
    {
        return Err(Error::BadRequest(
            "update endpoint is not an http(s) URL".into(),
        ));
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        return Err(Error::BadRequest(
            "an update operation is already running".into(),
        ));
    }
    hydrate(state).await;
    let now = crate::now_unix();
    let persisted = load_persisted(state).await;

    // A restart must not re-check what was checked within a day, and automatic checks stop
    // entirely while the API is in backoff.
    if !manual
        && (freshness_ok(now, &persisted)
            || persisted
                .rate_limited_until
                .is_some_and(|until| until > now))
    {
        set_phase("idle");
        BUSY.store(false, Ordering::SeqCst);
        return Ok(snapshot(state).await);
    }

    set_phase("checking");
    {
        let mut status = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        status.error = None;
        status.last_error = None;
    }

    let backoff = persisted.rate_limited_until.filter(|until| *until > now);
    let mut rate_limited_until = backoff;
    // A caller that arrives during backoff is told when the API may be used again.
    let mut api_problem: Option<String> = backoff.map(|until| rate_limit_message(until, now));
    let mut discovery: Option<Discovery> = None;

    if let (Some(token), None) = (token, backoff) {
        match check_api(state, endpoints, token, &persisted, now).await {
            Ok(ApiResult::Fresh { release, version }) => {
                apply_api_release(&release, &version, now);
                discovery = Some(Discovery::Api(release));
            }
            // A 304 only confirms a release this process already knows; with nothing
            // cached it says nothing, so discovery continues through the redirect.
            Ok(ApiResult::NotModified)
                if STATUS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .latest
                    .is_some() =>
            {
                {
                    let mut status = STATUS.lock().unwrap_or_else(|e| e.into_inner());
                    status.checked_at = Some(now);
                    status.rate_limited_until = None;
                    status.error = None;
                    status.last_error = None;
                    status.phase = Some("idle".into());
                }
                persist(state).await;
                BUSY.store(false, Ordering::SeqCst);
                return Ok(snapshot(state).await);
            }
            Ok(ApiResult::NotModified) => {}
            Ok(ApiResult::RateLimited { until }) => {
                rate_limited_until = Some(until);
                tracing::warn!(
                    retry_in_secs = until.saturating_sub(now),
                    "GitHub API rate limited; using the release redirect until the reset"
                );
                api_problem = Some(rate_limit_message(until, now));
            }
            Err(error) => {
                api_problem = Some(error.to_string());
            }
        }
    }

    if discovery.is_none() {
        match discover_via_redirect(state, endpoints).await {
            Ok(tag) => {
                let version = tag.trim_start_matches('v').to_string();
                {
                    let mut status = STATUS.lock().unwrap_or_else(|e| e.into_inner());
                    // Keep cached notes: the redirect carries no release body.
                    status.latest = Some(version.clone());
                    status.release_url = Some(format!(
                        "{}/{}",
                        endpoints.release_tag_base.trim_end_matches('/'),
                        tag
                    ));
                    status.checked_at = Some(now);
                    status.error = api_problem.clone();
                    status.last_error = api_problem.clone();
                    status.rate_limited_until = rate_limited_until;
                    status.phase = Some("idle".into());
                    if status
                        .downloaded
                        .as_ref()
                        .is_some_and(|_| !newer(env!("CARGO_PKG_VERSION"), &version))
                    {
                        status.downloaded = None;
                    }
                }
                discovery = Some(Discovery::Tag(tag));
            }
            Err(error) => {
                // Fallback failed too: keep the previous release, report one clean error.
                {
                    let mut status = STATUS.lock().unwrap_or_else(|e| e.into_inner());
                    status.rate_limited_until = rate_limited_until;
                    status.last_error =
                        Some(api_problem.clone().unwrap_or_else(|| error.to_string()));
                    status.error = status.last_error.clone();
                    status.phase = Some("idle".into());
                }
                persist(state).await;
                BUSY.store(false, Ordering::SeqCst);
                return Err(Error::Transport(
                    api_problem.unwrap_or_else(|| error.to_string()),
                ));
            }
        }
    }
    persist(state).await;

    if let Some(Discovery::Api(release)) = &discovery {
        let Some(version) = release.get("tag_name").and_then(Value::as_str) else {
            BUSY.store(false, Ordering::SeqCst);
            return Err(Error::Transport("release has no tag_name".into()));
        };
        let version = version.trim_start_matches('v').to_string();
        if !newer(env!("CARGO_PKG_VERSION"), &version) {
            set_phase("idle");
            BUSY.store(false, Ordering::SeqCst);
            return Ok(snapshot(state).await);
        }
        if download && deployment().can_download {
            let result = download_release(state, release, &version).await;
            BUSY.store(false, Ordering::SeqCst);
            result?;
            return Ok(snapshot(state).await);
        }
        set_phase("idle");
        BUSY.store(false, Ordering::SeqCst);
        return Ok(snapshot(state).await);
    }
    if let Some(Discovery::Tag(tag)) = &discovery {
        let version = tag.trim_start_matches('v').to_string();
        if download && deployment().can_download && newer(env!("CARGO_PKG_VERSION"), &version) {
            let result = download_tag(state, endpoints, tag, &version).await;
            BUSY.store(false, Ordering::SeqCst);
            result?;
            return Ok(snapshot(state).await);
        }
    }
    set_phase("idle");
    BUSY.store(false, Ordering::SeqCst);
    Ok(snapshot(state).await)
}

/// Copy the fields the status screen shows from a GitHub release payload.
fn apply_api_release(release: &Value, version: &str, now: i64) {
    let notes = release
        .get("body")
        .and_then(Value::as_str)
        .map(|body| body.chars().take(8_000).collect::<String>());
    let published = release
        .get("published_at")
        .and_then(Value::as_str)
        .map(str::to_string);
    let url = release
        .get("html_url")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut status = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    status.latest = Some(version.to_string());
    status.notes = notes;
    status.published_at = published;
    status.release_url = url;
    status.checked_at = Some(now);
    status.rate_limited_until = None;
    status.error = None;
    status.last_error = None;
    status.phase = Some("idle".into());
    if status
        .downloaded
        .as_ref()
        .is_some_and(|_| !newer(env!("CARGO_PKG_VERSION"), version))
    {
        status.downloaded = None;
    }
}

/// Download the archive when only the tag is known: the URL follows the release layout.
async fn download_tag(
    state: &AppState,
    endpoints: &Endpoints,
    tag: &str,
    version: &str,
) -> Result<()> {
    let asset_name = target_asset().ok_or_else(|| {
        Error::BadRequest("no prebuilt archive exists for this OS/architecture".into())
    })?;
    let url = download_url(tag, &asset_name, &endpoints.download_base);
    let checksum = format!("{url}.sha256");
    download_and_stage(state, &asset_name, version, &url, &checksum).await
}

async fn download_release(state: &AppState, release: &Value, version: &str) -> Result<()> {
    let asset_name = target_asset().ok_or_else(|| {
        Error::BadRequest("no prebuilt archive exists for this OS/architecture".into())
    })?;
    let Some(assets) = release.get("assets").and_then(Value::as_array) else {
        return Err(Error::Transport("release has no assets".into()));
    };
    let asset = assets
        .iter()
        .find(|a| a.get("name").and_then(Value::as_str) == Some(&asset_name))
        .ok_or_else(|| Error::Transport(format!("release asset {asset_name} is missing")))?;
    let Some(url) = asset.get("browser_download_url").and_then(Value::as_str) else {
        return Err(Error::Transport("release asset has no download URL".into()));
    };
    let checksum = format!("{url}.sha256");
    download_and_stage(state, &asset_name, version, url, &checksum).await
}

/// Fetch the archive and its checksum, verify SHA-256, and stage the binary. The URLs are
/// explicit so the API path can use GitHub's asset URLs and the redirect path can construct
/// the release-download layout.
async fn download_and_stage(
    state: &AppState,
    asset_name: &str,
    version: &str,
    url: &str,
    checksum_url: &str,
) -> Result<()> {
    set_phase("downloading");
    let bytes = state
        .http
        .get(url)
        .header("user-agent", "rrouter")
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| Error::Transport(format!("download: {e}")))?
        .error_for_status()
        .map_err(|e| Error::Transport(format!("download status: {e}")))?
        .bytes()
        .await
        .map_err(|e| Error::Transport(format!("download body: {e}")))?;
    let checksum = state
        .http
        .get(checksum_url)
        .header("user-agent", "rrouter")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| Error::Transport(format!("checksum: {e}")))?
        .error_for_status()
        .map_err(|e| Error::Transport(format!("checksum status: {e}")))?
        .text()
        .await
        .map_err(|e| Error::Transport(format!("checksum body: {e}")))?;
    let expected = checksum
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let actual = hex::encode(Sha256::digest(&bytes));
    if expected.is_empty() || expected != actual {
        return Err(Error::BadRequest(format!(
            "checksum mismatch for {asset_name}: expected {expected}, got {actual}"
        )));
    }

    let dir = state.config.load().data_dir.join("updates").join(version);
    std::fs::create_dir_all(&dir).map_err(|e| Error::Store(format!("create update dir: {e}")))?;
    let staged = dir.join("rrouter");
    let tmp = dir.join("rrouter.part");
    extract_binary(&bytes, &tmp).map_err(|e| Error::Store(format!("extract {asset_name}: {e}")))?;
    std::fs::rename(&tmp, &staged).map_err(|e| Error::Store(format!("stage binary: {e}")))?;
    {
        let mut status = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        status.downloaded = Some(staged);
        status.phase = Some("ready".into());
        status.error = None;
    }
    Ok(())
}

/// Unpack exactly one `rrouter` binary from the release tarball. Entry paths are
/// ignored; only the file name matters, so `../` in an archive cannot escape the target.
fn extract_binary(archive: &[u8], target: &Path) -> std::io::Result<()> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.file_name().and_then(|n| n.to_str()) != Some("rrouter") {
            continue;
        }
        let mut out = std::fs::File::create(target)?;
        std::io::copy(&mut entry, &mut out)?;
        drop(out);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))?;
        }
        return Ok(());
    }
    Err(std::io::Error::other(
        "archive did not contain a rrouter binary",
    ))
}

/// Perform the upgrade using the deployment's own mechanism. Binary/systemd replaces the
/// executable and execs; brew runs the formula upgrade; everything else reports the
/// command its supervisor owns.
pub async fn apply(state: &AppState) -> Result<Value> {
    let deployment = deployment();
    let current = env!("CARGO_PKG_VERSION");
    let latest;
    let staged;
    {
        let status = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        latest = status.latest.clone().unwrap_or_else(|| current.to_string());
        staged = status.downloaded.clone();
    }
    if !newer(current, &latest) {
        return Err(Error::BadRequest(
            "already running the latest version".into(),
        ));
    }
    if let Ok(command) = std::env::var("RROUTER_UPDATE_COMMAND") {
        if !command.trim().is_empty() {
            return run_command(&command).await;
        }
    }
    match deployment.kind.as_str() {
        "binary" | "systemd" => {
            let staged = staged
                .ok_or_else(|| Error::BadRequest("download the update before restarting".into()))?;
            self_replace(state, &staged)?;
            Ok(json!({
                "accepted": true,
                "restarting": true,
                "message": "the new binary is staged; the process restarts in a moment",
            }))
        }
        "brew" => run_command(&format!("brew upgrade {BREW_FORMULA}")).await,
        _ => Err(Error::BadRequest(format!(
            "{} manages this deployment; run: {}",
            deployment.label,
            deployment.command.unwrap_or_default()
        ))),
    }
}

async fn run_command(command: &str) -> Result<Value> {
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .await
        .map_err(|e| Error::Store(format!("run update command: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(Error::BadRequest(format!(
            "update command failed ({}): {}",
            output.status,
            stderr.trim()
        )));
    }
    Ok(json!({
        "accepted": true,
        "restarting": false,
        "message": stdout.trim(),
        "warning": stderr.trim(),
    }))
}

/// Replace the running executable and exec it after the HTTP response can be flushed.
fn self_replace(state: &AppState, staged: &Path) -> Result<()> {
    let exe =
        std::env::current_exe().map_err(|e| Error::Store(format!("current executable: {e}")))?;
    let dir = exe
        .parent()
        .ok_or_else(|| Error::Store("executable has no parent directory".into()))?
        .to_path_buf();
    let temp = dir.join(".rrouter.update");
    std::fs::copy(staged, &temp).map_err(|e| {
        Error::Store(format!(
            "cannot write {} (is the install directory writable?): {e}",
            temp.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755));
    }
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let data_dir = state.config.load().data_dir.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        if let Err(e) = std::fs::rename(&temp, &exe) {
            tracing::error!(error = %e, "update: replace executable failed");
            return;
        }
        let _ = data_dir;
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let error = std::process::Command::new(&exe).args(args).exec();
            tracing::error!(error = %error, "update: exec failed");
        }
    });
    Ok(())
}

/// The daily scheduler. Off by default is not useful here: checking is read-only and
/// cheap, so it starts enabled; downloading and applying stay opt-in.
pub fn spawn_daily(state: &std::sync::Arc<AppState>) {
    static START: std::sync::Once = std::sync::Once::new();
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    START.call_once(|| {
        let state = std::sync::Arc::clone(state);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            loop {
                let options = crate::settings::load(&state)
                    .await
                    .unwrap_or_else(|_| json!({}));
                if options
                    .get("update_auto_check")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
                {
                    let download = options
                        .get("update_auto_download")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if let Err(e) = check_automatic(&state, download).await {
                        tracing::warn!(error = %e, "automatic update check failed");
                    }
                }
                tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_compares_tags_and_prereleases() {
        assert!(newer("0.2.3", "v0.2.4"));
        assert!(newer("0.2.3", "0.3.0"));
        assert!(!newer("0.2.3", "v0.2.3"));
        assert!(!newer("0.2.4", "0.2.3"));
        assert!(newer("1.0.0-rc.1", "1.0.0"));
        assert!(!newer("1.0.0", "1.0.0-rc.1"));
    }

    #[test]
    fn target_assets_follow_the_release_names() {
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            let asset = target_asset().expect("desktop asset");
            assert!(asset.ends_with(".tar.gz"));
            assert!(asset.starts_with("rrouter-"));
        }
    }

    #[test]
    fn extracts_only_the_binary_from_a_release_archive() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut gz);
            let body = b"#!/bin/sh\nexit 0\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, "rrouter-linux-amd64/rrouter", &body[..])
                .expect("append binary");
            tar.finish().expect("finish archive");
        }
        let archive = gz.finish().expect("gzip");
        let dir = std::env::temp_dir().join(format!("rrouter-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let target = dir.join("rrouter");
        extract_binary(&archive, &target).expect("extract");
        assert_eq!(
            std::fs::read(&target).expect("read"),
            b"#!/bin/sh\nexit 0\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn deployment_override_is_honored() {
        // Serialized env mutation is avoided: just assert the pure mapping.
        assert!(deployment_for("docker").command.is_some());
        assert!(!deployment_for("helm").automatic);
        assert!(deployment_for("binary").automatic);
    }
}
