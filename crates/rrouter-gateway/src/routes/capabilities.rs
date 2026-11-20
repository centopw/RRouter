//! `GET /api/capabilities` — which providers serve each non-chat endpoint, and the
//! operator's stored default for it.
//!
//! Admin-only: the per-provider account counts are operator data, so the route lives on
//! the admin router rather than the read-only dashboard surface. The capability lists
//! mirror the gates the request path applies, so a default the page offers can actually
//! be served: search by `search_spec`, rerank by `rerank_endpoint`, embeddings by the
//! dialect `embeddings_inner` accepts.

use std::sync::Arc;

use axum::extract::State;
use axum::response::Response;
use rrouter_core::Dialect;
use rrouter_store::Connection;
use serde_json::{json, Value};

use super::{admin_error, ok_json};
use crate::settings::{self, CapabilityDefault};
use crate::state::AppState;

/// `GET /api/capabilities`.
pub async fn list(State(state): State<Arc<AppState>>) -> Response {
    let connections = match state.store.list_connections(None).await {
        Ok(connections) => connections,
        Err(e) => return admin_error(&e),
    };
    let defaults = json!({
        "search": settings::capability_default(CapabilityDefault::Search),
        "rerank": settings::capability_default(CapabilityDefault::Rerank),
        "embeddings": settings::capability_default(CapabilityDefault::Embeddings),
    });
    ok_json(aggregate(defaults, &connections))
}

/// One provider row before it is serialized: kept typed so the sort is on the facts the
/// wire carries rather than on a JSON projection of them.
struct Row {
    provider: &'static str,
    display: &'static str,
    default_model: Option<&'static str>,
    accounts: usize,
}

impl Row {
    fn into_value(self) -> Value {
        json!({
            "provider": self.provider,
            "display": self.display,
            "default_model": self.default_model,
            "connected": self.accounts > 0,
            "accounts": self.accounts,
        })
    }
}

/// The aggregate body for `defaults` and the instance's connections. Pure so the shape
/// and the active-only counting stay testable apart from the store.
fn aggregate(defaults: Value, connections: &[Connection]) -> Value {
    let mut search = Vec::new();
    let mut rerank = Vec::new();
    let mut embeddings = Vec::new();
    for spec in rrouter_auth::registry::all() {
        // Only active connections name a provider; a disabled account is kept in the
        // store but serves nothing, so it must not mark the provider connected.
        let accounts = connections
            .iter()
            .filter(|conn| conn.is_active && conn.provider == spec.id)
            .count();
        if let Some(found) = rrouter_auth::registry::search_spec(spec.id) {
            search.push(Row {
                provider: spec.id,
                display: spec.display,
                default_model: Some(found.default_model),
                accounts,
            });
        }
        if rrouter_auth::registry::rerank_endpoint(spec.id).is_some() {
            rerank.push(Row {
                provider: spec.id,
                display: spec.display,
                default_model: None,
                accounts,
            });
        }
        // The same eligibility gate `embeddings_inner` applies before it dispatches.
        if matches!(
            spec.default_dialect,
            Dialect::OpenAiChat | Dialect::OpenAiEmbeddings
        ) {
            embeddings.push(Row {
                provider: spec.id,
                display: spec.display,
                default_model: None,
                accounts,
            });
        }
    }
    json!({
        "defaults": defaults,
        "providers": {
            "search": rows(search),
            "rerank": rows(rerank),
            "embeddings": rows(embeddings),
        },
    })
}

/// Sort by display name, then provider id (community manifests can share a display).
fn rows(mut rows: Vec<Row>) -> Vec<Value> {
    rows.sort_by(|a, b| (a.display, a.provider).cmp(&(b.display, b.provider)));
    rows.into_iter().map(Row::into_value).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rrouter_auth::Credentials;
    use uuid::Uuid;

    fn connection(provider: &str, active: bool) -> Connection {
        Connection {
            id: Uuid::new_v4(),
            provider: provider.to_string(),
            auth_type: "api_key".into(),
            name: None,
            email: None,
            priority: 0,
            weight: 1,
            is_active: active,
            credentials: Credentials::default(),
            backoff_level: 0,
            last_error: None,
            last_error_code: None,
            last_error_at: None,
            refusal: None,
            team_id: None,
            owner_user_id: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn find<'a>(list: &'a Value, provider: &str) -> &'a Value {
        list.as_array()
            .expect("array")
            .iter()
            .find(|row| row["provider"] == json!(provider))
            .unwrap_or_else(|| panic!("{provider} missing from {list}"))
    }

    /// Search rows carry the model their spec would use, counts include only active
    /// connections, and the stored defaults are echoed unchanged.
    #[test]
    fn aggregate_lists_capable_providers_with_active_counts() {
        let connections = vec![
            connection("claude", true),
            connection("claude", true),
            connection("claude", false),
            connection("openai", true),
        ];
        let defaults = json!({"search": "claude", "rerank": null, "embeddings": null});
        let out = aggregate(defaults.clone(), &connections);
        assert_eq!(out["defaults"], defaults, "defaults echo the settings blob");

        let search = &out["providers"]["search"];
        let claude = find(search, "claude");
        assert_eq!(claude["default_model"], json!("claude-sonnet-4-5"));
        assert!(
            claude["display"]
                .as_str()
                .is_some_and(|name| !name.is_empty()),
            "a row carries its provider's display name"
        );
        assert_eq!(claude["connected"], json!(true));
        assert_eq!(claude["accounts"], json!(2), "only active rows count");

        let displays: Vec<&str> = search
            .as_array()
            .expect("search")
            .iter()
            .filter_map(|row| row["display"].as_str())
            .collect();
        let mut sorted = displays.clone();
        sorted.sort_unstable();
        assert_eq!(displays, sorted, "sorted by display");

        // Anthropic's dialect is not embeddings-eligible; OpenAI's is.
        let embeddings = &out["providers"]["embeddings"];
        assert!(embeddings
            .as_array()
            .expect("embeddings")
            .iter()
            .all(|row| row["provider"] != json!("claude")));
        let openai = find(embeddings, "openai");
        assert_eq!(openai["default_model"], Value::Null);
        assert_eq!(openai["connected"], json!(true));
        assert_eq!(openai["accounts"], json!(1));

        // A compiled provider with no connection is listed but not connected.
        let groq = find(embeddings, "groq");
        assert_eq!(groq["connected"], json!(false));
        assert_eq!(groq["accounts"], json!(0));

        // Rerank is manifest-driven; a core provider without the endpoint is absent.
        let rerank = &out["providers"]["rerank"];
        assert!(rerank
            .as_array()
            .expect("rerank")
            .iter()
            .all(|row| row["provider"] != json!("openai")));
    }
}
