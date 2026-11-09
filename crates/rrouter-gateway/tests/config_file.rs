//! `--check-config` and `--print-config` against the real binary.
//!
//! The unit tests in `config.rs` cover precedence and validation; these prove the CLI
//! contract an operator (or a deploy script) actually depends on: exit codes, the
//! source-annotated error, and that a referenced secret never reaches stdout.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_rrouter")
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rrouter-config-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Run the binary with a clean environment, so an ambient `RROUTER_*` variable in the
/// developer's shell can never change what these tests assert.
fn run(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(bin());
    command.env_clear().args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    command.output().expect("run rrouter")
}

fn text(out: &[u8]) -> String {
    String::from_utf8_lossy(out).into_owned()
}

#[test]
fn check_config_accepts_a_valid_file_and_reports_where_values_came_from() {
    let dir = temp_dir("ok");
    let file = dir.join("rrouter.json");
    std::fs::write(&file, r#"{"cache":"sqlite","max_concurrency":4}"#).expect("write config");
    let path = file.to_str().expect("utf-8 path");

    let out = run(
        &["--check-config"],
        &[("RROUTER_CONFIG", path), ("RROUTER_STRATEGY", "cheapest")],
    );

    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("config ok"), "{stdout}");
    assert!(stdout.contains(path), "{stdout}");
    assert!(
        stdout.contains("1 from the environment, 2 from the file"),
        "{stdout}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_config_without_a_file_uses_the_environment_and_says_so() {
    let out = run(&["--check-config"], &[("RROUTER_CACHE", "off")]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("RROUTER_CONFIG is unset"), "{stdout}");
    assert!(stdout.contains("1 from the environment"), "{stdout}");
}

#[test]
fn check_config_fails_with_a_source_annotated_error() {
    let dir = temp_dir("bad-key");
    let file = dir.join("rrouter.json");
    std::fs::write(&file, "{\n  \"prot\": 8080\n}").expect("write config");
    let path = file.to_str().expect("utf-8 path");

    let out = run(&["--check-config"], &[("RROUTER_CONFIG", path)]);

    assert!(!out.status.success(), "stdout: {}", text(&out.stdout));
    assert_eq!(
        out.status.code(),
        Some(1),
        "a config error is a clean exit 1"
    );
    let stderr = text(&out.stderr);
    assert!(stderr.contains("invalid configuration"), "{stderr}");
    assert!(stderr.contains(path), "the file must be named: {stderr}");
    assert!(stderr.contains("prot"), "the key must be named: {stderr}");
    assert!(
        stderr.contains("line 2"),
        "the line must be named: {stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn check_config_refuses_a_bind_address_from_the_file() {
    let dir = temp_dir("bind");
    let file = dir.join("rrouter.json");
    std::fs::write(&file, r#"{"port":8081}"#).expect("write config");
    let path = file.to_str().expect("utf-8 path");

    let out = run(&["--check-config"], &[("RROUTER_CONFIG", path)]);

    assert!(!out.status.success());
    let stderr = text(&out.stderr);
    assert!(stderr.contains("RROUTER_PORT"), "{stderr}");
    assert!(stderr.contains("startup"), "{stderr}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn print_config_reports_sources_and_never_prints_the_secret() {
    let dir = temp_dir("print");
    let file = dir.join("rrouter.json");
    std::fs::write(
        &file,
        r#"{"admin_token":{"key_env":"ADMIN_TOKEN"},"max_attempts":2}"#,
    )
    .expect("write config");
    let path = file.to_str().expect("utf-8 path");
    let secret = "secret-token-that-must-not-be-printed";

    let out = run(
        &["--print-config"],
        &[
            ("RROUTER_CONFIG", path),
            ("ADMIN_TOKEN", secret),
            ("RROUTER_MAX_ATTEMPTS", "3"),
        ],
    );

    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(!stdout.contains(secret), "secret leaked: {stdout}");
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(value["admin_token"], "set");
    assert_eq!(value["max_attempts"], 3);
    assert_eq!(value["config_file"], path);
    assert_eq!(value["sources"]["max_attempts"], "env");
    assert_eq!(value["sources"]["admin_token"], "file");
    assert_eq!(value["sources"]["port"], "default");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn print_config_without_a_config_file_keeps_the_defaults() {
    let out = run(&["--print-config"], &[]);
    assert!(out.status.success(), "stderr: {}", text(&out.stderr));
    let value: serde_json::Value = serde_json::from_str(&text(&out.stdout)).expect("json");
    assert_eq!(value["port"], 20128);
    assert_eq!(value["strategy"], "priority");
    assert_eq!(value["cache"], "off");
    assert!(
        !Path::new(value["data_dir"].as_str().expect("data_dir")).is_absolute(),
        "the default data dir is relative to the working directory"
    );
    assert_eq!(value["config_file"], serde_json::Value::Null);
    assert_eq!(value["sources"]["port"], "default");
}
