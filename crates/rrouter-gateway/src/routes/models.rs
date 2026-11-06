//! `/v1/models` and the alias / disabled-model admin routes.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::response::Response;
use rrouter_core::Error;
use serde_json::{json, Value};

use super::{admin_error, body_json, ok_json};
use crate::state::AppState;

/// Union of catalog models, aliases, combos, and every connected provider's models.
pub async fn list_models(State(state): State<Arc<AppState>>) -> Response {
    let mut entries: BTreeMap<String, String> = BTreeMap::new();

    for info in state.catalog.all() {
        entries.entry(info.id).or_insert(info.provider);
    }
    if let Ok(connections) = state.store.list_connections(None).await {
        for conn in connections {
            if !conn.is_active {
                continue;
            }
            // Catalog entries are keyed by model; keep only this connection's provider.
            for info in state.catalog.all() {
                if info.provider == conn.provider {
                    entries.entry(info.id).or_insert(info.provider);
                }
            }
        }
    }
    if let Ok(aliases) = state.store.list_aliases().await {
        for alias in aliases {
            entries.entry(alias.alias).or_insert_with(|| "rrouter".to_string());
        }
    }
    if let Ok(combos) = state.store.list_combos().await {
        for combo in combos {
            entries.entry(combo.name).or_insert_with(|| "rrouter".to_string());
        }
    }

    let created = crate::now_unix();
    let data: Vec<Value> = entries
        .into_iter()
        .map(|(id, owned_by)| {
            json!({ "id": id, "object": "model", "owned_by": owned_by, "created": created })
        })
        .collect();
    ok_json(json!({ "object": "list", "data": data }))
}

pub async fn list_aliases(State(state): State<Arc<AppState>>) -> Response {
    match state.store.list_aliases().await {
        Ok(aliases) => {
            let data: Vec<Value> = aliases
                .into_iter()
                .map(|a| json!({ "alias": a.alias, "target": a.target }))
                .collect();
            ok_json(json!({ "data": data }))
        }
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/aliases` with `{alias, target}`.
pub async fn create_alias(State(state): State<Arc<AppState>>, body: Body) -> Response {
    upsert_alias(&state, body).await
}

/// `PUT /api/aliases` with `{alias, target}`.
pub async fn update_alias(State(state): State<Arc<AppState>>, body: Body) -> Response {
    upsert_alias(&state, body).await
}

async fn upsert_alias(state: &AppState, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let (Some(alias), Some(target)) = (str_field(&value, "alias"), str_field(&value, "target"))
    else {
        return admin_error(&Error::BadRequest("alias and target are required".into()));
    };
    match state.store.upsert_alias(&alias, &target).await {
        Ok(()) => ok_json(json!({ "alias": alias, "target": target })),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/aliases?alias=x` (or `{alias}` in the body).
pub async fn delete_alias(State(state): State<Arc<AppState>>, req: axum::extract::Request) -> Response {
    let alias = match alias_from_request(req).await {
        Ok(a) => a,
        Err(resp) => return *resp,
    };
    match state.store.delete_alias(&alias).await {
        Ok(true) => ok_json(json!({ "deleted": true, "alias": alias })),
        Ok(false) => admin_error(&Error::NotFound(format!("alias {alias}"))),
        Err(e) => admin_error(&e),
    }
}

async fn alias_from_request(req: axum::extract::Request) -> Result<String, Box<Response>> {
    let query = req.uri().query().unwrap_or("");
    if let Some(alias) = query_param(query, "alias") {
        return Ok(alias);
    }
    let (_, body) = req.into_parts();
    let value = body_json(body).await?;
    str_field(&value, "alias")
        .ok_or_else(|| Box::new(admin_error(&Error::BadRequest("alias is required".into()))))
}

pub async fn disable(State(state): State<Arc<AppState>>, Path(model): Path<String>) -> Response {
    set_disabled(&state, &model, true).await
}

pub async fn enable(State(state): State<Arc<AppState>>, Path(model): Path<String>) -> Response {
    set_disabled(&state, &model, false).await
}

async fn set_disabled(state: &AppState, model: &str, disabled: bool) -> Response {
    match state.store.set_disabled(model, disabled).await {
        Ok(()) => ok_json(json!({ "model": model, "disabled": disabled })),
        Err(e) => admin_error(&e),
    }
}

pub(crate) fn str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Minimal `?k=v&k2=v2` reader, percent-decoding `%XX` sequences.
pub(crate) fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (percent_decode(k) == key).then(|| percent_decode(v))
    })
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_params_decode() {
        assert_eq!(query_param("a=1&model=claude%2Fsonnet-4-5", "model").as_deref(), Some("claude/sonnet-4-5"));
        assert_eq!(query_param("alias=", "alias").as_deref(), Some(""));
        assert_eq!(query_param("x=1", "alias"), None);
    }
}
