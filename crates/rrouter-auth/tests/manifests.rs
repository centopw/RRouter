//! Round-trip from a reviewed manifest to the compiled provider table.
//!
//! `build.rs` compiles `providers/community/*.json` into `COMMUNITY_PROVIDERS` using the
//! validator in `rrouter_auth::manifest`. These tests close the loop: they parse the same
//! files and require the compiled `ProviderSpec` to carry exactly what the manifest says —
//! so a field the compiler silently drops fails here instead of at a provider's 400.

use std::path::{Path, PathBuf};

use rrouter_auth::manifest::{self, Auth, DialectName, Manifest, MaxTokensName};
use rrouter_auth::registry::{self, AuthStyle, ProviderSpec};
use rrouter_core::Dialect;
use rrouter_translate::{MaxTokensKey, WireProfile};

fn community_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers/community")
}

/// Every reviewed manifest on disk, newest content included.
fn manifests() -> Vec<(PathBuf, Manifest)> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(community_dir())
        .expect("providers/community exists")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "at least Azure OpenAI and one OpenAI-compatible provider must ship"
    );
    paths
        .into_iter()
        .map(|path| {
            let raw = std::fs::read_to_string(&path).expect("readable manifest");
            let value: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
            let manifest =
                manifest::parse(&value).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            (path, manifest)
        })
        .collect()
}

fn dialect_of(name: DialectName) -> Dialect {
    match name {
        DialectName::Chat => Dialect::OpenAiChat,
        DialectName::Responses => Dialect::OpenAiResponses,
        DialectName::Messages => Dialect::AnthropicMessages,
    }
}

fn auth_style_of(auth: Auth) -> AuthStyle {
    match auth {
        Auth::Bearer => AuthStyle::Bearer,
        Auth::XApiKey => AuthStyle::XApiKey,
        Auth::ApiKeyHeader => AuthStyle::ApiKeyHeader,
        Auth::None => AuthStyle::None,
    }
}

fn max_tokens_of(name: MaxTokensName) -> MaxTokensKey {
    match name {
        MaxTokensName::MaxTokens => MaxTokensKey::MaxTokens,
        MaxTokensName::MaxCompletionTokens => MaxTokensKey::MaxCompletionTokens,
    }
}

fn pairs(values: &[(&str, &str)]) -> Vec<(String, String)> {
    values
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

/// Every field `build.rs` writes must equal what the manifest declares.
fn assert_compiled(manifest: &Manifest, spec: &ProviderSpec) {
    let at = |field: &str| format!("{}: {field}", manifest.id);
    assert_eq!(spec.display, manifest.display, "{}", at("display"));
    assert_eq!(spec.base_url, manifest.base_url, "{}", at("base_url"));
    assert_eq!(
        spec.base_url_requires_override,
        manifest.base_url_requires_override,
        "{}",
        at("base_url_requires_override")
    );
    assert_eq!(
        spec.responses_url.map(str::to_owned),
        manifest.responses_url,
        "{}",
        at("responses_url")
    );
    assert_eq!(
        spec.models_url.map(str::to_owned),
        manifest.models_url,
        "{}",
        at("models_url")
    );
    assert_eq!(
        spec.messages_url.map(str::to_owned),
        manifest.messages_url,
        "{}",
        at("messages_url")
    );
    assert_eq!(
        spec.rerank_url().map(str::to_owned),
        manifest.rerank_url,
        "{}",
        at("rerank_url")
    );
    assert_eq!(
        spec.files_url().map(str::to_owned),
        manifest.files_url,
        "{}",
        at("files_url")
    );
    assert_eq!(
        spec.batches_url().map(str::to_owned),
        manifest.batches_url,
        "{}",
        at("batches_url")
    );
    assert_eq!(
        spec.images_url.map(str::to_owned),
        manifest.images_url,
        "{}",
        at("images_url")
    );
    assert_eq!(
        spec.audio_speech_url.map(str::to_owned),
        manifest.audio_speech_url,
        "{}",
        at("audio_speech_url")
    );
    assert_eq!(
        spec.audio_transcriptions_url.map(str::to_owned),
        manifest.audio_transcriptions_url,
        "{}",
        at("audio_transcriptions_url")
    );
    assert_eq!(
        spec.default_dialect,
        dialect_of(manifest.default_dialect),
        "{}",
        at("default_dialect")
    );
    assert_eq!(spec.auth, auth_style_of(manifest.auth), "{}", at("auth"));
    assert_eq!(
        spec.auth_header,
        manifest.auth_header,
        "{}",
        at("auth_header")
    );
    assert_eq!(
        spec.default_max_tokens.map(u64::from),
        manifest.default_max_tokens,
        "{}",
        at("default_max_tokens")
    );
    assert_eq!(
        spec.stream_only,
        manifest.stream_only,
        "{}",
        at("stream_only")
    );
    assert_eq!(
        pairs(spec.extra_headers),
        manifest.extra_headers,
        "{}",
        at("extra_headers")
    );
    assert_eq!(pairs(spec.query), manifest.query, "{}", at("query"));
    assert_eq!(
        spec.max_tokens_key,
        max_tokens_of(manifest.max_tokens_key),
        "{}",
        at("max_tokens_key")
    );
    assert_eq!(
        spec.passthrough_headers
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>(),
        manifest.passthrough_headers,
        "{}",
        at("passthrough_headers")
    );
}

#[test]
fn every_manifest_compiles_field_for_field() {
    for (path, manifest) in manifests() {
        let spec = registry::get(&manifest.id)
            .unwrap_or_else(|| panic!("{}: id {} is not compiled", path.display(), manifest.id));
        assert_compiled(&manifest, spec);
    }
}

#[test]
fn the_core_table_is_unchanged_and_community_providers_are_added() {
    // Wave 1 shipped 15; Wave 15B added the Antigravity OAuth row, the OpenCode Go plan is
    // its own transport, and Qoder's COSY-signed agent endpoint is its own dialect. A
    // manifest may add providers but never rewrite one.
    assert_eq!(registry::PROVIDERS.len(), 18, "core provider count");
    let community = registry::all().count() - registry::PROVIDERS.len();
    assert_eq!(
        community,
        manifests().len(),
        "one compiled spec per manifest"
    );
    let ids: Vec<&str> = registry::all().map(|spec| spec.id).collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "provider ids must be unique");
}

#[test]
fn azure_is_a_first_class_api_key_provider() {
    let path = community_dir().join("azure.json");
    let raw = std::fs::read_to_string(&path).expect("azure manifest");
    let manifest = manifest::parse(&serde_json::from_str(&raw).expect("json")).expect("valid");

    assert_eq!(manifest.auth, Auth::ApiKeyHeader);
    assert_eq!(manifest.auth_header, "api-key");
    let spec = registry::get("azure").expect("azure is compiled");
    assert_eq!(spec.auth, AuthStyle::ApiKeyHeader);

    // The key rides in `api-key`, never in Authorization.
    let creds = rrouter_auth::Credentials {
        api_key: Some("azure-key".into()),
        base_url_override: Some("https://contoso.openai.azure.com".into()),
        ..rrouter_auth::Credentials::default()
    };
    let headers = rrouter_auth::build_headers(spec, &creds).expect("headers");
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    assert_eq!(header("api-key"), Some("azure-key"));
    assert_eq!(header("authorization"), None);

    // OpenAI's own surface, so the modern cap spelling; the deployment is the model id.
    assert_eq!(spec.max_tokens_key, MaxTokensKey::MaxCompletionTokens);
    assert_eq!(
        spec.wire_profile(),
        WireProfile::new(MaxTokensKey::MaxCompletionTokens, None)
    );
    assert_eq!(spec.default_dialect, Dialect::OpenAiChat);
    // v1 GA: no api-version is required, so none is declared.
    assert!(spec.query.is_empty());
    // A tenant placeholder, not a tenant: the operator supplies the resource, and the spec
    // says so instead of relying on the operator reading the notes.
    assert!(spec
        .base_url
        .starts_with("https://RESOURCE.openai.azure.com/openai/v1/"));
    assert!(spec.base_url_requires_override);
    assert!(rrouter_auth::manifest::is_placeholder_host(spec.base_url));
}

/// The review finding this flag exists for: an Azure connection created without a resource
/// URL must fail loudly, before any DNS lookup, and must not be savable unnoticed.
#[test]
fn azure_is_unusable_until_the_connection_supplies_its_resource() {
    let spec = registry::get("azure").expect("azure is compiled");
    let anonymous = rrouter_auth::Credentials {
        api_key: Some("azure-key".into()),
        ..rrouter_auth::Credentials::default()
    };
    let err = rrouter_auth::build_headers(spec, &anonymous)
        .expect_err("dispatch must refuse the placeholder host");
    assert_eq!(err.status(), 401);
    assert!(
        err.to_string().contains("requires a connection base URL"),
        "{err}"
    );

    // The save-time validator the connection route must call.
    assert!(rrouter_auth::validate_base_url_override(spec, None).is_err());
    assert!(
        rrouter_auth::validate_base_url_override(spec, Some("https://RESOURCE.openai.azure.com"))
            .is_err(),
        "copying the placeholder is the same mistake"
    );
    assert!(rrouter_auth::validate_base_url_override(
        spec,
        Some("https://contoso.openai.azure.com")
    )
    .is_ok());
}

#[test]
fn only_the_manifest_that_declares_a_placeholder_requires_an_override() {
    assert!(
        registry::get("azure")
            .expect("azure")
            .base_url_requires_override
    );
    for id in ["fireworks", "mistral", "openai", "claude", "ollama"] {
        let spec = registry::get(id).expect(id);
        assert!(
            !spec.base_url_requires_override,
            "{id} has a real endpoint and must not demand an override"
        );
        assert!(
            rrouter_auth::validate_base_url_override(spec, None).is_ok(),
            "{id} must be usable without an override"
        );
    }
}

#[test]
fn an_openai_compatible_provider_compiles_with_its_documented_wire_shape() {
    let spec = registry::get("fireworks").expect("fireworks is compiled");
    assert_eq!(spec.auth, AuthStyle::Bearer);
    assert_eq!(
        spec.base_url,
        "https://api.fireworks.ai/inference/v1/chat/completions"
    );
    assert_eq!(spec.default_dialect, Dialect::OpenAiChat);
    // Fireworks documents `max_tokens`, not `max_completion_tokens`.
    assert_eq!(spec.max_tokens_key, MaxTokensKey::MaxTokens);
    assert_eq!(spec.max_tokens_key.as_str(), "max_tokens");
    assert!(spec.query.is_empty());
}

/// The two rerank providers exist for `/v1/rerank`; their endpoints are the documented
/// ones, not invented, and no core provider gained one.
#[test]
fn cohere_and_jina_ship_their_rerank_endpoints() {
    for (id, url) in [
        ("cohere", "https://api.cohere.com/v1/rerank"),
        ("jina", "https://api.jina.ai/v1/rerank"),
    ] {
        let spec = registry::get(id).expect(id);
        assert_eq!(spec.rerank_url(), Some(url), "{id}");
        assert_eq!(registry::rerank_endpoint(id), Some(url), "{id}");
        assert_eq!(spec.auth, AuthStyle::Bearer, "{id}");
        let rerank = spec.rerank_url().expect("rerank url");
        assert!(rerank.starts_with("https://"), "{id}");
        assert!(!rerank.ends_with('/'), "{id}");
        assert!(!rerank.contains('?'), "{id}");
    }

    // Chat keeps working where the vendor documents an OpenAI-compatible surface.
    assert_eq!(
        registry::get("cohere").expect("cohere").base_url,
        "https://api.cohere.ai/compatibility/v1/chat/completions"
    );
    for spec in registry::all().filter(|spec| spec.rerank_url().is_some()) {
        assert!(
            matches!(spec.id, "cohere" | "jina"),
            "{} declares a rerank endpoint",
            spec.id
        );
    }
}

#[test]
fn a_rerank_endpoint_is_optional_for_every_other_provider() {
    for id in ["openai", "mistral", "claude", "azure", "fireworks"] {
        assert_eq!(
            registry::get(id).expect(id).rerank_url(),
            None,
            "{id} serves no rerank endpoint"
        );
        assert_eq!(registry::rerank_endpoint(id), None, "{id}");
    }
    // The generated table names only registered providers, exactly once, and matches the
    // manifests one for one (an entry that drifted from its manifest fails here).
    let files = manifests();
    let mut declared: Vec<&str> = files
        .iter()
        .filter_map(|(_, manifest)| manifest.rerank_url.as_ref().map(|_| manifest.id.as_str()))
        .collect();
    declared.sort_unstable();
    let mut compiled: Vec<&str> = registry::COMMUNITY_RERANK
        .iter()
        .map(|(id, _)| *id)
        .collect();
    compiled.sort_unstable();
    assert_eq!(declared, compiled, "generated rerank table drifted");
    for (id, url) in registry::COMMUNITY_FILES {
        assert!(
            registry::get(id).is_some(),
            "{id} is not a registered provider"
        );
        assert!(url.starts_with("https://"), "{id}");
    }
    for (id, url) in registry::COMMUNITY_BATCHES {
        assert!(
            registry::get(id).is_some(),
            "{id} is not a registered provider"
        );
        assert!(url.starts_with("https://"), "{id}");
    }
    for (id, url) in registry::COMMUNITY_RERANK {
        assert!(
            registry::get(id).is_some(),
            "{id} is not a registered provider"
        );
        assert!(url.starts_with("https://"), "{id}");
    }
    // The manifest contract accepts it, rejects a non-https or trailing-slash form, and the
    // build refuses an unknown key beside it.
    let mut value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(community_dir().join("jina.json")).expect("jina manifest"),
    )
    .expect("json");
    value["rerank_url"] = serde_json::json!("http://api.jina.ai/v1/rerank");
    assert!(manifest::parse(&value).is_err());
    value["rerank_url"] = serde_json::json!("https://api.jina.ai/v1/rerank/");
    assert!(manifest::parse(&value).is_err());
    value["rerank_url"] = serde_json::json!("https://api.jina.ai/v1/rerank");
    value["rerank_format"] = serde_json::json!("cohere");
    assert!(
        manifest::parse(&value).is_err(),
        "unknown keys stay rejected"
    );
}

#[test]
fn an_unknown_key_in_a_manifest_is_rejected() {
    let mut value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(community_dir().join("fireworks.json")).expect("manifest"),
    )
    .expect("json");
    value["max_completion_tokens"] = serde_json::json!("max_completion_tokens");

    let err = manifest::parse(&value).expect_err("unknown keys must fail the build");
    assert!(err.contains("unknown key `max_completion_tokens`"), "{err}");
}

#[test]
fn a_manifest_cannot_ship_a_credential() {
    for (path, _) in manifests() {
        let raw = std::fs::read_to_string(&path).expect("manifest");
        let lower = raw.to_ascii_lowercase();
        for needle in ["sk-", "ghp_", "aiza", "bearer ", "-----begin"] {
            assert!(
                !lower.contains(needle),
                "{} contains what looks like a credential: {needle}",
                path.display()
            );
        }
    }
}

#[test]
fn an_unreviewed_id_is_still_unknown() {
    // Nothing is registered at runtime: only the compiled table resolves.
    assert!(registry::get("azure").is_some());
    assert!(registry::get("not-a-provider").is_none());
}

/// No JSON-Schema validator ships with this repo, so the build's validator *is* the
/// enforcement. This test keeps it honest against the published schema: the two must name
/// the same keys and the same closed enums, or a manifest that validates by hand fails the
/// build (and vice versa).
#[test]
fn the_published_schema_matches_the_build_validator() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers/schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("schema")).expect("json");

    let properties: std::collections::BTreeSet<&str> = schema["properties"]
        .as_object()
        .expect("properties")
        .keys()
        .map(String::as_str)
        .collect();
    let known: std::collections::BTreeSet<&str> = manifest::KNOWN_KEYS.iter().copied().collect();
    assert_eq!(properties, known, "schema and validator key sets diverge");
    assert_eq!(schema["additionalProperties"], serde_json::json!(false));

    let required: Vec<&str> = schema["required"]
        .as_array()
        .expect("required")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert_eq!(
        required,
        vec!["id", "display", "base_url", "auth", "default_dialect"]
    );

    let enum_of = |pointer: &str| -> Vec<&str> {
        schema
            .pointer(pointer)
            .and_then(serde_json::Value::as_array)
            .expect("enum")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect()
    };
    assert_eq!(
        enum_of("/properties/auth/enum"),
        vec!["bearer", "x-api-key", "api-key", "none"]
    );
    assert_eq!(
        enum_of("/properties/default_dialect/enum"),
        vec!["chat", "responses", "messages"]
    );
    assert_eq!(
        enum_of("/properties/max_tokens_key/enum"),
        vec!["max_tokens", "max_completion_tokens"]
    );
    assert_eq!(
        enum_of("/properties/passthrough_headers/items/enum"),
        vec!["anthropic-beta", "openai-beta"]
    );
}
