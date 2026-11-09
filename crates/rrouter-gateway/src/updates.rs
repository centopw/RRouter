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
const RELEASE_PAGE: &str = "https://github.com/centopw/RRouter/releases";
const BREW_FORMULA: &str = "rrouter";

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
    json!({
        "current_version": env!("CARGO_PKG_VERSION"),
        "latest_version": status.latest,
        "release_url": status.release_url.clone().unwrap_or_else(|| RELEASE_PAGE.to_string()),
        "notes": status.notes,
        "published_at": status.published_at,
        "checked_at": status.checked_at,
        "phase": status.phase.clone().unwrap_or_else(|| "idle".into()),
        "error": status.error,
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

fn set_error(error: &str) {
    let mut state = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    state.phase = Some("error".into());
    state.error = Some(error.to_string());
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

/// Check GitHub and optionally download the matching release archive.
pub async fn check(state: &std::sync::Arc<AppState>, download: bool) -> Result<Value> {
    if BUSY.swap(true, Ordering::SeqCst) {
        return Err(Error::BadRequest(
            "an update operation is already running".into(),
        ));
    }
    set_phase("checking");
    {
        let mut status = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        status.error = None;
    }
    let result = check_inner(state, download).await;
    BUSY.store(false, Ordering::SeqCst);
    if let Err(error) = &result {
        set_error(&error.to_string());
    }
    match result {
        Ok(()) => Ok(snapshot(state).await),
        Err(e) => Err(e),
    }
}

async fn check_inner(state: &AppState, download: bool) -> Result<()> {
    let response = state
        .http
        .get(RELEASE_API)
        .header("accept", "application/vnd.github+json")
        .header("user-agent", "rrouter")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| Error::Transport(format!("update check: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Upstream {
            provider: "github".into(),
            status: status.as_u16(),
            body: response.text().await.unwrap_or_default(),
            payload: None,
        });
    }
    let release: Value = response
        .json()
        .await
        .map_err(|e| Error::Transport(format!("update release body: {e}")))?;
    let tag = release
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Transport("release has no tag_name".into()))?
        .to_string();
    let version = tag.trim_start_matches('v').to_string();
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
    let current = env!("CARGO_PKG_VERSION");

    {
        let mut state = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        state.latest = Some(version.clone());
        state.notes = notes;
        state.published_at = published;
        state.release_url = url;
        state.checked_at = Some(crate::now_unix());
        state.error = None;
        if state
            .downloaded
            .as_ref()
            .is_some_and(|_| !newer(current, &version))
        {
            state.downloaded = None;
        }
    }

    if !newer(current, &version) {
        set_phase("idle");
        return Ok(());
    }
    if !download || !deployment().can_download {
        set_phase("idle");
        return Ok(());
    }
    download_release(state, &release, &version).await
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
        .get(format!("{url}.sha256"))
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
                    if let Err(e) = check(&state, download).await {
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
