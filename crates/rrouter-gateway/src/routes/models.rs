//! `/v1/models`, the model registry admin routes, and the alias / disabled-model routes.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use rrouter_auth::ProviderSpec;
use rrouter_core::{ChatRequest, ChatResponse, Dialect, Error, Message, Part, Role};
use rrouter_store::{Connection, ModelRow};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{admin_error, body_json, ok_json};
use crate::models_registry::{self, SOURCE_CUSTOM, SOURCE_UPSTREAM};
use crate::state::{AppState, Caller};

/// One `/v1/models` entry before preset grouping: the id, who owns it, and the registry
/// row that describes its limits and capabilities.
struct ListedModel {
    id: String,
    provider: String,
    meta: Option<models_registry::ModelItem>,
}

/// Models a client can actually call: the registry (already limited to connected
/// providers) plus aliases and combos. Group grants filter both explicit groups and
/// their models; implicit provider buckets stay open.
pub async fn list_models(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let caller = match state.authenticate(&headers).await {
        Ok(caller) => caller,
        Err(e) => {
            return crate::error::error_response(Dialect::OpenAiChat, &e);
        }
    };
    // One entry per id. A model row carries the operator's preferred provider for that
    // bare id; use it for `owned_by` only when that provider actually serves the id.
    let registry = match models_registry::list(&state).await {
        Ok(items) => items,
        Err(e) => return admin_error(&e),
    };
    let mut entries: BTreeMap<String, (String, Option<String>, bool)> = BTreeMap::new();
    for item in &registry {
        // A disabled model is not callable and must not be advertised. Other
        // enabled providers of the same id still list it below.
        if !item.enabled {
            continue;
        }
        let entry = entries.entry(item.id.clone()).or_insert((
            item.provider.clone(),
            item.preferred_provider.clone(),
            false,
        ));
        if let Some(preferred) = &item.preferred_provider {
            entry.1 = Some(preferred.clone());
            if item.provider == *preferred {
                entry.2 = true;
            }
        }
    }

    // Enrichment is resolved from the row that actually owns the id. A bare id served by
    // several providers uses the same provider the client is routed to.
    let by_ref: HashMap<String, models_registry::ModelItem> = registry
        .iter()
        .filter(|item| item.enabled)
        .map(|item| (item.reference.clone(), item.clone()))
        .collect();
    let by_id: HashMap<String, models_registry::ModelItem> = registry
        .iter()
        .filter(|item| item.enabled)
        .fold(HashMap::new(), |mut map, item| {
            map.entry(item.id.clone()).or_insert_with(|| item.clone());
            map
        });
    // Names the gateway owns are not registry rows. An explicit entry (even `None`)
    // stops the generic lookup from borrowing an unrelated model's metadata.
    let mut owned_meta: HashMap<String, Option<models_registry::ModelItem>> = HashMap::new();

    if let Ok(aliases) = state.store.list_aliases().await {
        for alias in aliases {
            let owned = entries
                .entry(alias.alias.clone())
                .or_insert_with(|| ("rrouter".to_string(), None, false));
            // An alias that collides with a real model id keeps the model's routing and
            // metadata; only the names the gateway owns borrow a target's metadata.
            if owned.0 == "rrouter" {
                let meta = resolve_meta(&alias.target, &by_ref, &by_id).map(without_display);
                owned_meta.insert(alias.alias, meta);
            }
        }
    }
    if let Ok(combos) = state.store.list_combos().await {
        for combo in combos {
            let owned = entries
                .entry(combo.name.clone())
                .or_insert_with(|| ("rrouter".to_string(), None, false));
            if owned.0 == "rrouter" {
                owned_meta.insert(combo.name, combo_meta(&combo.models, &by_ref, &by_id));
            }
        }
    }

    let created = crate::now_unix();
    let models: Vec<ListedModel> = entries
        .into_iter()
        .map(|(id, (fallback, preferred, preferred_serves))| {
            let owned_by = if preferred_serves {
                preferred.unwrap_or(fallback)
            } else {
                fallback
            };
            let meta = match owned_meta.remove(&id) {
                Some(explicit) => explicit,
                None => by_ref
                    .get(&format!("{owned_by}/{id}"))
                    .cloned()
                    .or_else(|| by_id.get(&id).cloned()),
            };
            ListedModel {
                id,
                provider: owned_by,
                meta,
            }
        })
        .collect();
    let (data, groups) = grouped_models(&state, created, models, &caller).await;
    ok_json(json!({ "object": "list", "data": data, "groups": groups }))
}

/// Resolve an alias target or combo member to its registry row.
fn resolve_meta(
    reference: &str,
    by_ref: &HashMap<String, models_registry::ModelItem>,
    by_id: &HashMap<String, models_registry::ModelItem>,
) -> Option<models_registry::ModelItem> {
    by_ref
        .get(reference)
        .cloned()
        .or_else(|| {
            reference
                .split_once('/')
                .filter(|(provider, _)| rrouter_auth::registry::get(provider).is_some())
                .and_then(|(_, id)| by_id.get(id).cloned())
        })
        .or_else(|| by_id.get(reference).cloned())
}

fn without_display(mut meta: models_registry::ModelItem) -> models_registry::ModelItem {
    meta.display = None;
    meta
}

/// A combo can land on any member, so advertise only what every member supports and the
/// smallest limits. Unknown members make the whole answer unknown.
fn combo_meta(
    members: &[String],
    by_ref: &HashMap<String, models_registry::ModelItem>,
    by_id: &HashMap<String, models_registry::ModelItem>,
) -> Option<models_registry::ModelItem> {
    let mut out: Option<models_registry::ModelItem> = None;
    for member in members {
        let meta = without_display(resolve_meta(member, by_ref, by_id)?);
        out = Some(match out {
            None => meta,
            Some(current) => intersect_meta(current, meta),
        });
    }
    out
}

fn intersect_meta(
    mut left: models_registry::ModelItem,
    right: models_registry::ModelItem,
) -> models_registry::ModelItem {
    left.context = min_limit(left.context, right.context);
    left.max_output = min_limit(left.max_output, right.max_output);
    left.capabilities = left.capabilities.intersect(right.capabilities);
    left.capabilities_extra = if left.capabilities_extra == right.capabilities_extra {
        left.capabilities_extra
    } else {
        None
    };
    left.effort = if left.effort == right.effort {
        left.effort
    } else {
        None
    };
    left.api_capabilities = if left.api_capabilities == right.api_capabilities {
        left.api_capabilities
    } else {
        None
    };
    left.deprecated |= right.deprecated;
    left.display = None;
    left.cost = None;
    left
}

fn min_limit(left: Option<u32>, right: Option<u32>) -> Option<u32> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        _ => None,
    }
}

/// Apply the default preset's groups and visibility to the flat `/v1/models` list.
///
/// The OpenAI shape stays a flat `data` array. Grouping is additive metadata: every entry
/// gains `group` (a group id) and the response gains an ordered `groups` array. The
/// default preset is the discovery policy: an item explicitly hidden is omitted, a group
/// toggled off is omitted with its models, and `auto_include_new: false` drops models the
/// preset never named. Routing is unaffected; hidden models still serve requests by name.
async fn grouped_models(
    state: &AppState,
    created: i64,
    models: Vec<ListedModel>,
    caller: &Caller,
) -> (Vec<Value>, Vec<Value>) {
    let layout = state
        .store
        .default_model_preset()
        .await
        .ok()
        .flatten()
        .map(|preset| preset.layout);
    let auto_include = layout
        .as_ref()
        .map(|layout| layout.auto_include_new)
        .unwrap_or(true);
    let empty = Vec::new();
    let explicit_groups: &[rrouter_store::LayoutGroup] = layout
        .as_ref()
        .map(|layout| layout.groups.as_slice())
        .unwrap_or(&empty);
    let group_by_id: HashMap<&str, &rrouter_store::LayoutGroup> = explicit_groups
        .iter()
        .map(|group| (group.id.as_str(), group))
        .collect();
    let provider_groups: HashMap<&str, &str> = explicit_groups
        .iter()
        .filter_map(|group| group.provider.as_deref().map(|p| (p, group.id.as_str())))
        .collect();
    // A group grant scopes discovery: its models and bucket disappear for callers that
    // do not match. Implicit provider buckets have no grant and stay open.
    let denied: HashSet<&str> = explicit_groups
        .iter()
        .filter(|group| {
            !rrouter_store::LayoutDocument::allows(
                group,
                caller.key_id(),
                caller.team_id(),
                caller.owner_user_id(),
            )
        })
        .map(|group| group.id.as_str())
        .collect();
    let item_by_ref: HashMap<String, &rrouter_store::LayoutItem> = layout
        .as_ref()
        .map(|layout| {
            layout
                .items
                .iter()
                .map(|item| (format!("{}\0{}", item.provider, item.id), item))
                .collect()
        })
        .unwrap_or_default();

    // A model joins its explicit group, else the explicit provider bucket, else an
    // implicit provider bucket. Items and groups hidden from discovery are dropped here,
    // before ordering, so they cannot contribute an empty section.
    let mut resolved: Vec<(String, String, String, u32)> = Vec::with_capacity(models.len());
    let mut used_groups: HashSet<String> = HashSet::new();
    let mut implicit: Vec<&str> = Vec::new();
    for model in &models {
        let id = &model.id;
        let provider = &model.provider;
        let item = item_by_ref.get(&format!("{provider}\0{id}"));
        if !item.map(|item| item.visible).unwrap_or(auto_include) {
            continue;
        }
        let group = item
            .and_then(|item| item.group.as_deref())
            .filter(|group| group_by_id.contains_key(group))
            .map(str::to_string)
            .or_else(|| {
                provider_groups
                    .get(provider.as_str())
                    .map(|id| (*id).to_string())
            })
            .unwrap_or_else(|| {
                let id = format!("provider:{provider}");
                if !implicit.contains(&provider.as_str()) {
                    implicit.push(provider.as_str());
                }
                id
            });
        if group_by_id
            .get(group.as_str())
            .is_some_and(|group| !group.visible)
        {
            continue;
        }
        if denied.contains(group.as_str()) {
            continue;
        }
        used_groups.insert(group.clone());
        let position = item
            .map(|item| {
                if item.visible {
                    item.position
                } else {
                    item.last_visible_position.unwrap_or(u32::MAX)
                }
            })
            .unwrap_or(u32::MAX);
        resolved.push((id.clone(), provider.clone(), group, position));
    }

    // Explicit groups keep their stored order; implicit provider buckets follow in
    // first-seen order. Only groups with a listed model are advertised.
    let mut group_order: Vec<(String, String, Option<String>)> = Vec::new();
    for group in explicit_groups {
        if !used_groups.contains(&group.id) {
            continue;
        }
        group_order.push((group.id.clone(), group.name.clone(), group.provider.clone()));
    }
    for provider in implicit {
        let id = format!("provider:{provider}");
        if !used_groups.contains(&id) || group_order.iter().any(|(existing, _, _)| *existing == id)
        {
            continue;
        }
        let name = if provider == "rrouter" {
            "RRouter".to_string()
        } else {
            rrouter_auth::registry::get(provider)
                .map(|spec| spec.display.to_string())
                .unwrap_or_else(|| provider.to_string())
        };
        group_order.push((id, name, Some(provider.to_string())));
    }
    let index: HashMap<&str, usize> = group_order
        .iter()
        .enumerate()
        .map(|(index, (id, _, _))| (id.as_str(), index))
        .collect();

    resolved.sort_by(|a, b| {
        let a_index = index.get(a.2.as_str()).copied().unwrap_or(usize::MAX);
        let b_index = index.get(b.2.as_str()).copied().unwrap_or(usize::MAX);
        a_index
            .cmp(&b_index)
            .then(a.3.cmp(&b.3))
            .then(a.0.cmp(&b.0))
    });

    let groups: Vec<Value> = group_order
        .iter()
        .enumerate()
        .map(|(position, (id, name, provider))| {
            json!({
                "id": id,
                "name": name,
                "provider": provider,
                "position": position,
            })
        })
        .collect();
    let meta_by_id: HashMap<&str, &models_registry::ModelItem> = models
        .iter()
        .filter_map(|model| model.meta.as_ref().map(|meta| (model.id.as_str(), meta)))
        .collect();
    let group_meta: HashMap<&str, (&str, Option<&str>)> = group_order
        .iter()
        .map(|(id, name, provider)| (id.as_str(), (name.as_str(), provider.as_deref())))
        .collect();
    let data = resolved
        .into_iter()
        .map(|(id, provider, group, _)| {
            let (canonical, bare) = if provider == "rrouter" {
                (id.clone(), None)
            } else {
                let (name, source) = group_meta
                    .get(group.as_str())
                    .copied()
                    .unwrap_or((provider.as_str(), Some(provider.as_str())));
                // A provider bucket keeps the stable provider id; a free-form group is
                // addressed by its name, which is the alias the planner resolves.
                let qualifier = if source.is_some() {
                    provider.as_str()
                } else {
                    name
                };
                (format!("{qualifier}/{id}"), Some(id.clone()))
            };
            model_json(
                &canonical,
                bare.as_deref(),
                &provider,
                created,
                Some(&group),
                meta_by_id.get(id.as_str()).copied(),
            )
        })
        .collect();
    (data, groups)
}

/// The OpenAI model object plus the additive capability metadata clients use to pick a
/// model. Unknown fields are omitted, never invented.
fn model_json(
    id: &str,
    bare: Option<&str>,
    owned_by: &str,
    created: i64,
    group: Option<&str>,
    meta: Option<&models_registry::ModelItem>,
) -> Value {
    let mut object = Map::new();
    object.insert("id".into(), Value::String(id.to_string()));
    // The bare id remains available to clients that need it, without a second request.
    if let Some(bare) = bare {
        object.insert("model".into(), Value::String(bare.to_string()));
    }
    object.insert("object".into(), Value::String("model".into()));
    object.insert("owned_by".into(), Value::String(owned_by.to_string()));
    object.insert("created".into(), Value::Number(created.into()));
    if let Some(group) = group {
        object.insert("group".into(), Value::String(group.to_string()));
    }
    let Some(meta) = meta else {
        return Value::Object(object);
    };
    if let Some(name) = &meta.display {
        object.insert("name".into(), Value::String(name.clone()));
    }
    if let Some(context) = meta.context {
        object.insert("context_window".into(), Value::Number(context.into()));
        object.insert("context_length".into(), Value::Number(context.into()));
    }
    if let Some(max_output) = meta.max_output {
        object.insert("max_output_tokens".into(), Value::Number(max_output.into()));
        object.insert(
            "max_completion_tokens".into(),
            Value::Number(max_output.into()),
        );
    }
    let caps = meta.capabilities;
    // Modalities are emitted only when something is actually known. A bare upstream id
    // with no limits and no modality flag must not claim text-only or audio support.
    if meta.context.is_some()
        || meta.max_output.is_some()
        || caps.vision
        || caps.pdf
        || caps.audio_input
        || caps.video_input
        || caps.image_output
        || caps.audio_output
    {
        let mut input = vec!["text"];
        if caps.vision {
            input.push("image");
        }
        if caps.pdf {
            input.push("pdf");
        }
        if caps.audio_input {
            input.push("audio");
        }
        if caps.video_input {
            input.push("video");
        }
        let mut output = vec!["text"];
        if caps.image_output {
            output.push("image");
        }
        if caps.audio_output {
            output.push("audio");
        }
        object.insert("input_modalities".into(), json!(input));
        object.insert("output_modalities".into(), json!(output));
    }
    let mut capabilities = Map::new();
    for (key, flag) in [
        ("vision", caps.vision),
        ("pdf", caps.pdf),
        ("audioInput", caps.audio_input),
        ("videoInput", caps.video_input),
        ("imageOutput", caps.image_output),
        ("audioOutput", caps.audio_output),
        ("search", caps.search),
        ("tools", caps.tools),
        ("reasoning", caps.reasoning),
    ] {
        capabilities.insert(key.into(), Value::Bool(flag));
    }
    if let Some(context) = meta.context {
        capabilities.insert("contextWindow".into(), Value::Number(context.into()));
    }
    if let Some(max_output) = meta.max_output {
        capabilities.insert("maxOutput".into(), Value::Number(max_output.into()));
    }
    if let Some(extra) = &meta.capabilities_extra {
        for (key, value) in extra {
            capabilities.insert(key.clone(), value.clone());
        }
    }
    object.insert("capabilities".into(), Value::Object(capabilities));
    if let Some(effort) = &meta.effort {
        object.insert("effort".into(), effort.clone());
    }
    if let Some(reasoning) = &meta.reasoning {
        object.insert(
            "reasoning".into(),
            serde_json::to_value(reasoning).unwrap_or(Value::Null),
        );
    }
    if let Some(api) = &meta.api_capabilities {
        object.insert("api_capabilities".into(), api.clone());
    }
    if meta.deprecated {
        object.insert("deprecated".into(), Value::Bool(true));
    }
    Value::Object(object)
}

/// `GET /v1/models/{id}`: the OpenAI model object for a routable `id` or `provider/id`.
///
/// Routable means the registry lists the model for a provider connected right now and
/// the model is not disabled; aliases and combos are routable names the gateway owns.
/// Anything else is the OpenAI 404 `model_not_found`, never a guess.
pub async fn get_model(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(reference): Path<String>,
) -> Response {
    let caller = match state.authenticate(&headers).await {
        Ok(caller) => caller,
        Err(e) => {
            return crate::error::error_response(Dialect::OpenAiChat, &e);
        }
    };
    let reference = reference.trim_start_matches('/');
    match routable_model(&state, reference, &caller).await {
        Ok(Some((owned_by, canonical, bare, meta))) => ok_json(model_json(
            &canonical,
            bare.as_deref(),
            &owned_by,
            crate::now_unix(),
            None,
            meta.as_ref(),
        )),
        Ok(None) => crate::error::error_response(
            Dialect::OpenAiChat,
            &Error::NotFound(format!("model {reference} is not available")),
        ),
        Err(e) => crate::error::error_response(Dialect::OpenAiChat, &e),
    }
}

/// What `GET /v1/models/{id}` resolved to: owner, advertised id, bare id, metadata.
type RoutedModel = (
    String,
    String,
    Option<String>,
    Option<models_registry::ModelItem>,
);

/// Which provider owns `reference` and what the gateway knows about the model, when it
/// can route the name. A free-form group prefix (`main/model`) resolves through the
/// default preset; a registered provider prefix stays authoritative.
async fn routable_model(
    state: &AppState,
    reference: &str,
    caller: &Caller,
) -> rrouter_core::Result<Option<RoutedModel>> {
    // `provider/id` only splits when the prefix is a registry provider: ids such as
    // `meta-llama/Llama-3-70B` carry a slash of their own.
    let (provider, bare) = match reference.split_once('/') {
        Some((prefix, rest)) if rrouter_auth::get(prefix).is_some() => (Some(prefix), rest),
        _ => (None, reference),
    };
    let items = models_registry::list(state).await?;
    if let Some(item) = items
        .iter()
        .find(|item| item.enabled && item.id == bare && provider.is_none_or(|p| item.provider == p))
    {
        return Ok(Some((
            item.provider.clone(),
            format!("{}/{}", item.provider, item.id),
            Some(item.id.clone()),
            Some(item.clone()),
        )));
    }

    // A free-form group name is a routing alias. Resolve only when the prefix is not a
    // registered provider, so provider ids never become ambiguous.
    if provider.is_none() {
        if let Some((head, rest)) = reference.split_once('/') {
            if let Some(preset) = state.store.default_model_preset().await? {
                if let Some(group) = preset.layout.group_by_name(head) {
                    if !rrouter_store::LayoutDocument::allows(
                        group,
                        caller.key_id(),
                        caller.team_id(),
                        caller.owner_user_id(),
                    ) {
                        return Ok(None);
                    }
                    let members = preset.layout.group_members(
                        items
                            .iter()
                            .filter(|item| item.enabled)
                            .map(|item| (item.provider.clone(), item.id.clone())),
                        group,
                        rest,
                    );
                    let preferred = items
                        .iter()
                        .find(|item| item.id == rest)
                        .and_then(|item| item.preferred_provider.as_deref());
                    let chosen = members
                        .iter()
                        .filter_map(|(provider, id)| {
                            items.iter().find(|item| {
                                item.enabled && &item.provider == provider && &item.id == id
                            })
                        })
                        .find(|item| preferred == Some(item.provider.as_str()))
                        .or_else(|| {
                            members.iter().find_map(|(provider, id)| {
                                items.iter().find(|item| {
                                    item.enabled && &item.provider == provider && &item.id == id
                                })
                            })
                        });
                    return Ok(chosen.map(|item| {
                        (
                            item.provider.clone(),
                            reference.to_string(),
                            Some(rest.to_string()),
                            Some(item.clone()),
                        )
                    }));
                }
            }
        }
    }

    // Alias and combo metadata comes from the registry rows they resolve to.
    let by_ref: HashMap<String, models_registry::ModelItem> = items
        .iter()
        .filter(|item| item.enabled)
        .map(|item| (item.reference.clone(), item.clone()))
        .collect();
    let by_id: HashMap<String, models_registry::ModelItem> = items
        .iter()
        .filter(|item| item.enabled)
        .fold(HashMap::new(), |mut map, item| {
            map.entry(item.id.clone()).or_insert_with(|| item.clone());
            map
        });
    if let Some(alias) = state
        .store
        .list_aliases()
        .await?
        .into_iter()
        .find(|alias| alias.alias == reference)
    {
        let meta = resolve_meta(&alias.target, &by_ref, &by_id).map(without_display);
        return Ok(Some((
            "rrouter".to_string(),
            reference.to_string(),
            None,
            meta,
        )));
    }
    if let Some(combo) = state
        .store
        .list_combos()
        .await?
        .into_iter()
        .find(|combo| combo.name == reference)
    {
        return Ok(Some((
            "rrouter".to_string(),
            reference.to_string(),
            None,
            combo_meta(&combo.models, &by_ref, &by_id),
        )));
    }
    Ok(None)
}

/// How long a connection create/update waits for the provider's model list before
/// detaching the fetch. Long enough for a normal catalogue, short enough that a slow
/// provider does not hold the admin response hostage.
pub const INITIAL_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Fetch a connection's models once, for a caller that already owns a task.
///
/// The registry lists a provider's real catalog without an extra click; a provider that
/// cannot enumerate (codex) or a failed fetch is logged and otherwise ignored — the
/// catalog rows remain.
pub async fn initial_fetch(state: Arc<AppState>, id: Uuid) {
    let Ok(Some(conn)) = state.store.get_connection(id).await else {
        return;
    };
    let Some(spec) = rrouter_auth::get(&conn.provider) else {
        return;
    };
    if !models_registry::can_list(spec.id) {
        return;
    }
    if let Err(e) = fetch_upstream(&state, spec, &conn).await {
        tracing::debug!(provider = %conn.provider, error = %e, "initial model fetch failed");
    }
}

/// Spawn [`initial_fetch`]; the returned handle is awaitable or droppable.
pub fn spawn_initial_fetch(state: Arc<AppState>, id: Uuid) -> tokio::task::JoinHandle<()> {
    tokio::spawn(initial_fetch(state, id))
}

/// Await the initial fetch for at most `bound`.
///
/// A timed-out handle is dropped, which detaches (not aborts) the spawned task: the
/// catalogue still lands a moment later and the admin request returns now.
pub async fn wait_initial_fetch_within(state: Arc<AppState>, id: Uuid, bound: std::time::Duration) {
    let handle = spawn_initial_fetch(state, id);
    if tokio::time::timeout(bound, handle).await.is_err() {
        tracing::debug!(
            connection = %id,
            "initial model fetch still running after the bound; detached"
        );
    }
}

/// The production bound for connection create/update and OAuth completion.
pub async fn wait_initial_fetch(state: Arc<AppState>, id: Uuid) {
    wait_initial_fetch_within(state, id, INITIAL_FETCH_TIMEOUT).await;
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
pub async fn delete_alias(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
) -> Response {
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

// ----- model registry --------------------------------------------------------------

/// `GET /api/models`: the merged registry, sorted by `provider`, then `id`.
pub async fn registry_list(State(state): State<Arc<AppState>>) -> Response {
    match models_registry::list(&state).await {
        Ok(items) => ok_json(json!({ "data": items })),
        Err(e) => admin_error(&e),
    }
}

/// `GET /api/models/preferences` — `{data:[{model,provider,updated_at}]}`.
pub async fn list_preferences(State(state): State<Arc<AppState>>) -> Response {
    match state.store.model_preferences().await {
        Ok(preferences) => ok_json(json!({ "data": preference_rows(&preferences) })),
        Err(e) => admin_error(&e),
    }
}

/// `PUT /api/models/preference` — `{model, provider}` sets; `provider:null` clears.
/// Returns the updated preference list plus a `{model: provider}` map.
pub async fn put_preference(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let Some(model) = value.get("model").and_then(Value::as_str) else {
        return admin_error(&Error::BadRequest("model is required".into()));
    };
    match value.get("provider") {
        Some(Value::Null) => {
            if let Err(e) = state.store.clear_model_preference(model).await {
                return admin_error(&e);
            }
        }
        Some(Value::String(provider)) => {
            if let Err(e) = state.store.set_model_preference(model, provider).await {
                return admin_error(&e);
            }
        }
        None => {
            return admin_error(&Error::BadRequest(
                "provider is required (null clears the preference)".into(),
            ))
        }
        Some(_) => {
            return admin_error(&Error::BadRequest(
                "provider must be a string or null".into(),
            ))
        }
    }
    match state.store.model_preferences().await {
        Ok(preferences) => {
            let map: Map<String, Value> = preferences
                .iter()
                .map(|preference| {
                    (
                        preference.model.clone(),
                        Value::String(preference.provider.clone()),
                    )
                })
                .collect();
            ok_json(json!({
                "data": preference_rows(&preferences),
                "preferences": map,
            }))
        }
        Err(e) => admin_error(&e),
    }
}

fn preference_rows(preferences: &[rrouter_store::ModelPreference]) -> Vec<Value> {
    preferences
        .iter()
        .map(|preference| {
            json!({
                "model": preference.model,
                "provider": preference.provider,
                "updated_at": preference.updated_at,
            })
        })
        .collect()
}

/// `GET /api/models/providers`: every routable provider, with whether it has an active
/// connection and whether it can enumerate its own models.
/// A copy-pasteable origin for the provider's base-URL field, or `None` when the manifest
/// declares no usable host.
///
/// For a placeholder manifest (`RESOURCE.openai.azure.com`) the first host label becomes
/// `<resource>`, so the example shows the *shape* to fill in instead of a value that the
/// save validation would refuse. Only the origin is returned: an override is an origin and
/// the gateway grafts the provider's endpoint path onto it (see `proxy::endpoint`).
fn base_url_example(spec: &rrouter_auth::ProviderSpec) -> Option<String> {
    let (scheme, rest) = spec.base_url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = authority.split(':').next()?;
    if host.is_empty() {
        return None;
    }
    let placeholder = spec.base_url_requires_override
        || rrouter_auth::manifest::is_placeholder_host(spec.base_url);
    if placeholder {
        let (_, tail) = host.split_once('.')?;
        return Some(format!("{scheme}://<resource>.{tail}"));
    }
    Some(format!("{scheme}://{host}"))
}

/// `GET /api/models/providers` — the provider catalog with this caller's reach.
///
/// `has_connection` and `scope` are computed from the connections the caller may see, so a
/// team account is never told that another team (or the instance) has an account it cannot
/// use. For global authority the answer is the whole instance, exactly as before.
pub async fn providers(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let scope = match super::teams::reader_teams(&state, &headers).await {
        Ok(scope) => scope,
        Err(response) => return *response,
    };
    let connections = match state.store.list_connections(None).await {
        Ok(connections) => connections,
        Err(e) => return admin_error(&e),
    };
    let mut active: BTreeSet<String> = BTreeSet::new();
    let mut shared: BTreeSet<String> = BTreeSet::new();
    for conn in connections
        .into_iter()
        .filter(|conn| conn.is_active && super::connections::in_scope(conn, &scope))
    {
        if conn.team_id.is_none() {
            shared.insert(conn.provider.clone());
        }
        active.insert(conn.provider);
    }
    let mut data: Vec<Value> = rrouter_auth::all()
        .map(|spec| {
            json!({
                "id": spec.id,
                "display": spec.display,
                // How this provider authenticates, so the dashboard can offer
                // "subscription" and "API key" as separate paths instead of guessing.
                "kind": match spec.kind {
                    rrouter_auth::ProviderKind::OAuth => "oauth",
                    rrouter_auth::ProviderKind::ApiKey => "api_key",
                    rrouter_auth::ProviderKind::Local => "local",
                },
                "has_connection": active.contains(spec.id),
                // `global` = a shared instance account the caller can see (serves
                // non-team keys); `team` = only a team-owned account is reachable;
                // `none` = nothing. A team account never sees a shared row, so it can
                // never be told `global` from the instance's own accounts.
                "scope": if shared.contains(spec.id) {
                    "global"
                } else if active.contains(spec.id) {
                    "team"
                } else {
                    "none"
                },
                "listed": models_registry::can_list(spec.id),
                // A provider whose manifest host is a placeholder (Azure OpenAI's
                // `RESOURCE.openai.azure.com`) cannot route until the connection supplies
                // its own resource URL, so the dashboard must ask for one.
                "requires_base_url_override": spec.base_url_requires_override,
                "base_url_example": base_url_example(spec),
            })
        })
        .collect();
    data.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    ok_json(json!({ "data": data }))
}

/// `POST /api/models/sync`: replace every `synced` row from models.dev.
pub async fn sync(State(state): State<Arc<AppState>>) -> Response {
    match models_registry::sync(&state).await {
        Ok((models, providers, synced_at)) => ok_json(json!({
            "ok": true,
            "models": models,
            "providers": providers,
            "synced_at": synced_at,
        })),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/models/fetch` with `{connection_id}`: store the provider's own model list
/// as `upstream` rows.
pub async fn fetch(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(raw_id) = str_field(&value, "connection_id") else {
        return admin_error(&Error::BadRequest("connection_id is required".into()));
    };
    let Ok(id) = Uuid::parse_str(&raw_id) else {
        return admin_error(&Error::BadRequest("connection_id must be a uuid".into()));
    };
    let conn = match state.store.get_connection(id).await {
        Ok(Some(conn)) => conn,
        Ok(None) => return admin_error(&Error::NotFound(format!("connection {id}"))),
        Err(e) => return admin_error(&e),
    };
    let Some(spec) = rrouter_auth::get(&conn.provider) else {
        return admin_error(&Error::BadRequest(format!(
            "unknown provider {}",
            conn.provider
        )));
    };
    if !models_registry::can_list(spec.id) {
        // Contract: 400 plus an explanation the UI can show next to the disabled button.
        return admin_error(&Error::BadRequest(format!(
            "{} has no model list endpoint; add models with POST /api/models/custom",
            spec.id
        )));
    }
    match fetch_upstream(&state, spec, &conn).await {
        Ok(payload) => ok_json(payload),
        Err(e) => admin_error(&e),
    }
}

async fn fetch_upstream(
    state: &AppState,
    spec: &'static ProviderSpec,
    conn: &Connection,
) -> rrouter_core::Result<Value> {
    let creds = state
        .refresher
        .fresh(state.store.as_ref(), spec, conn.id)
        .await?;

    // Google Antigravity publishes no `GET /models`: its catalogue is a project-scoped
    // `POST /v1internal:fetchAvailableModels` on the production Cloud Code Assist host.
    if spec.id == rrouter_auth::antigravity::PROVIDER_ID {
        let project = rrouter_auth::antigravity::project_id(&creds).ok_or_else(|| {
            Error::BadRequest(
                "google-antigravity connection is missing credentials.extra.project_id".into(),
            )
        })?;
        let token = creds
            .access_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                Error::Unauthorized("google-antigravity connection has no access token".to_string())
            })?;
        let (status, models) = fetch_cca_models(
            &state.http,
            &rrouter_auth::antigravity::prod_base(),
            token,
            project,
        )
        .await?;
        let items = models
            .into_iter()
            .map(|model| models_registry::FetchedModel {
                id: model.id,
                display: model.display,
                context: None,
                max_output: None,
                caps: Value::Object(Map::new()),
            })
            .collect();
        return store_fetched(state, conn, status, items).await;
    }

    // Qoder's catalogue is a COSY-signed, per-account `GET`; it is also the only source of
    // the `model_config` block every envelope must carry.
    if spec.id == rrouter_auth::qoder::PROVIDER_ID {
        let signer = rrouter_auth::qoder::signer(&creds)?;
        let base = creds
            .base_url_override
            .as_deref()
            .unwrap_or(spec.base_url)
            .trim_end_matches('/')
            .to_string();
        let catalog = rrouter_auth::qoder::catalog(&state.http, &signer, &base).await?;
        let items = catalog
            .entries()
            .map(|(key, block)| models_registry::FetchedModel {
                id: key.to_string(),
                display: block
                    .get("display_name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .filter(|name| !name.is_empty()),
                context: block
                    .get("max_input_tokens")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
                max_output: block
                    .get("max_output_tokens")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok()),
                caps: json!({
                    "vision": block.get("is_vl").and_then(Value::as_bool).unwrap_or(false),
                    "reasoning": block.get("is_reasoning").and_then(Value::as_bool).unwrap_or(false),
                    "tools": true,
                }),
            })
            .collect();
        return store_fetched(state, conn, 200, items).await;
    }

    let base = creds.base_url_override.as_deref().unwrap_or(spec.base_url);
    let mut url = match (creds.base_url_override.as_deref(), spec.models_url) {
        // An override points at a different deployment, so its catalogue wins.
        (Some(_), _) => models_registry::models_url(base),
        (None, Some(explicit)) => explicit.to_string(),
        (None, None) => models_registry::models_url(base),
    };
    // Anthropic's list paginates (default 20). Asking for the maximum page once avoids a
    // "latest models missing" list without implementing cursor paging this endpoint does
    // not need for its other providers.
    if matches!(conn.provider.as_str(), "claude" | "anthropic") && !url.contains('?') {
        url.push_str("?limit=1000");
    }
    let headers = provider_headers(spec, &creds)?;
    let response = state
        .http
        .get(&url)
        .headers(headers)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(transport_error)?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body: String = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(512)
            .collect();
        return Err(Error::Upstream {
            provider: conn.provider.clone(),
            status,
            body,
            payload: None,
        });
    }
    let body: Value = response.json().await.map_err(transport_error)?;
    store_fetched(state, conn, status, models_registry::parse_models(&body)).await
}

/// `POST <prod>/v1internal:fetchAvailableModels` with the account's Cloud project.
///
/// Kept independent of `AppState` so the request shape and parser are testable against a
/// mock host. `quotaInfo` is parsed past (the registry has no quota column); ids and
/// `displayName` become upstream rows.
async fn fetch_cca_models(
    http: &reqwest::Client,
    base: &str,
    access_token: &str,
    project: &str,
) -> rrouter_core::Result<(u16, Vec<rrouter_auth::antigravity::AvailableModel>)> {
    let url = format!(
        "{}{}",
        base.trim_end_matches('/'),
        rrouter_auth::antigravity::FETCH_MODELS_PATH
    );
    let response = http
        .post(&url)
        .bearer_auth(access_token)
        .header("accept", "application/json")
        .header("user-agent", rrouter_translate::google_cca::USER_AGENT)
        .header(
            "x-goog-api-client",
            rrouter_translate::google_cca::CLIENT_HEADER,
        )
        .json(&json!({ "project": project }))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(transport_error)?;
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let body: String = response
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(512)
            .collect();
        return Err(Error::Upstream {
            provider: rrouter_auth::antigravity::PROVIDER_ID.to_string(),
            status,
            body,
            payload: None,
        });
    }
    let body: Value = response.json().await.map_err(transport_error)?;
    Ok((status, rrouter_auth::antigravity::available_models(&body)))
}

/// Persist one connection's fetched catalogue as `upstream` rows and echo the result.
async fn store_fetched(
    state: &AppState,
    conn: &Connection,
    status: u16,
    mut items: Vec<models_registry::FetchedModel>,
) -> rrouter_core::Result<Value> {
    items.truncate(models_registry::MAX_FETCHED_IDS);

    let known: BTreeSet<(String, String)> = state
        .store
        .list_models()
        .await?
        .into_iter()
        .map(|row| (row.provider, row.model))
        .collect();
    let added = items
        .iter()
        .filter(|item| !known.contains(&(conn.provider.clone(), item.id.clone())))
        .count();
    for item in &items {
        state
            .store
            .upsert_model(&ModelRow {
                provider: conn.provider.clone(),
                model: item.id.clone(),
                display: item.display.clone(),
                context: item.context,
                max_output: item.max_output,
                caps: item.caps.clone(),
                // A provider's own list carries ids, not prices; models.dev supplies them.
                cost: None,
                source: SOURCE_UPSTREAM.to_string(),
                updated_at: 0,
            })
            .await?;
    }
    let ids: Vec<String> = items.into_iter().map(|item| item.id).collect();
    Ok(json!({
        "provider": conn.provider,
        "status": status,
        "count": ids.len(),
        "added": added,
        "data": ids,
    }))
}

/// `POST /api/models/custom`: upsert one operator-defined row.
pub async fn custom(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let (Some(provider), Some(id)) = (str_field(&value, "provider"), str_field(&value, "id"))
    else {
        return admin_error(&Error::BadRequest("provider and id are required".into()));
    };
    let row = ModelRow {
        provider: provider.clone(),
        model: id.clone(),
        display: str_field(&value, "display"),
        context: u32_field(&value, "context"),
        max_output: u32_field(&value, "max_output"),
        caps: capabilities_from(&value),
        cost: value.get("cost").cloned().filter(|c| c.is_object()),
        source: SOURCE_CUSTOM.to_string(),
        updated_at: 0,
    };
    if let Err(e) = state.store.upsert_model(&row).await {
        return admin_error(&e);
    }
    match models_registry::item(&state, &provider, &id).await {
        Ok(Some(item)) => ok_json(item),
        Ok(None) => admin_error(&Error::Store(format!(
            "custom row {provider}/{id} vanished"
        ))),
        Err(e) => admin_error(&e),
    }
}

/// `DELETE /api/models/custom?provider=…&id=…`: only a `custom` row may be removed this way.
pub async fn delete_custom(
    State(state): State<Arc<AppState>>,
    Query(query): Query<Vec<(String, String)>>,
) -> Response {
    let param = |key: &str| {
        query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.is_empty())
    };
    let (Some(provider), Some(id)) = (param("provider"), param("id")) else {
        return admin_error(&Error::BadRequest("provider and id are required".into()));
    };
    match state.store.delete_model(&provider, &id).await {
        Ok(true) => ok_json(json!({ "deleted": true })),
        Ok(false) => admin_error(&Error::NotFound(format!("custom model {provider}/{id}"))),
        Err(e) => admin_error(&e),
    }
}

/// `POST /api/models/test` with `{model}`: one real 16-token completion through the
/// planner and dispatcher the proxy uses.
pub async fn test(State(state): State<Arc<AppState>>, body: Body) -> Response {
    let value = match body_json(body).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(model) = str_field(&value, "model") else {
        return admin_error(&Error::BadRequest("model is required".into()));
    };
    let started = Instant::now();
    // Same entry point a client call takes, so "test" cannot pass on a code path the
    // proxy never runs (it did once: the probe bypassed forced streaming).
    let mut request = ChatRequest::new(model.clone());
    request.messages.push(Message::text(Role::User, "ping"));
    request.max_tokens = Some(16);
    let result = match rrouter_translate::build_request(Dialect::OpenAiChat, &request, false) {
        Ok(probe) => crate::proxy::complete(&state, Dialect::OpenAiChat, &probe).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(response) => ok_json(json!({
            "ok": true,
            "status": 200,
            "latency_ms": elapsed_ms(started),
            "model": model,
            "reply": reply_text(&response),
        })),
        Err(e) => test_failure(&e, started),
    }
}

/// A failed probe is still an HTTP 200: the UI renders both outcomes.
fn test_failure(error: &Error, started: Instant) -> Response {
    ok_json(json!({
        "ok": false,
        "status": error.status(),
        "latency_ms": elapsed_ms(started),
        "error": error.to_string(),
    }))
}

fn elapsed_ms(started: Instant) -> u32 {
    started.elapsed().as_millis().min(u32::MAX as u128) as u32
}

fn reply_text(response: &ChatResponse) -> String {
    response
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Request headers for one outbound provider call, auth included.
fn provider_headers(
    spec: &ProviderSpec,
    creds: &rrouter_auth::Credentials,
) -> rrouter_core::Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    for (name, value) in rrouter_auth::build_headers(spec, creds)? {
        if let (Ok(name), Ok(value)) = (
            axum::http::header::HeaderName::from_bytes(name.as_bytes()),
            axum::http::header::HeaderValue::from_str(&value),
        ) {
            headers.append(name, value);
        }
    }
    Ok(headers)
}

fn transport_error(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Timeout(e.to_string())
    } else {
        Error::Transport(e.to_string())
    }
}

/// The nine known capability keys the body stated; absent keys fall through to the row
/// this one overlays.
fn capabilities_from(value: &Value) -> Value {
    const KEYS: [&str; 9] = [
        "vision",
        "pdf",
        "audio_input",
        "video_input",
        "image_output",
        "audio_output",
        "tools",
        "reasoning",
        "search",
    ];
    let mut caps = Map::new();
    let Some(given) = value.get("capabilities").and_then(Value::as_object) else {
        return Value::Object(caps);
    };
    for key in KEYS {
        if let Some(flag) = given.get(key).and_then(Value::as_bool) {
            caps.insert(key.to_string(), Value::Bool(flag));
        }
    }
    Value::Object(caps)
}

fn u32_field(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
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
        assert_eq!(
            query_param("a=1&model=claude%2Fsonnet-4-5", "model").as_deref(),
            Some("claude/sonnet-4-5")
        );
        assert_eq!(query_param("alias=", "alias").as_deref(), Some(""));
        assert_eq!(query_param("x=1", "alias"), None);
    }

    /// The CCA catalogue is a project-scoped POST, not `GET /models`: the request must
    /// carry the Bearer token and `{project}`, and the `models` map keys become ids.
    #[tokio::test]
    async fn cca_fetch_posts_the_project_and_parses_the_catalogue() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1internal:fetchAvailableModels"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer at-antigravity",
            ))
            .and(wiremock::matchers::header(
                "user-agent",
                rrouter_translate::google_cca::USER_AGENT,
            ))
            .and(wiremock::matchers::body_json(json!({"project": "proj-1"})))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(json!({
                "models": {
                    "gemini-3-pro": {
                        "displayName": "Gemini 3 Pro",
                        "quotaInfo": {"remainingFraction": 0.75}
                    },
                    "gemini-2.5-flash": {"displayName": "Gemini 2.5 Flash"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let (status, models) = fetch_cca_models(
            &reqwest::Client::new(),
            &server.uri(),
            "at-antigravity",
            "proj-1",
        )
        .await
        .expect("catalogue");
        assert_eq!(status, 200);
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(ids, vec!["gemini-2.5-flash", "gemini-3-pro"]);
        assert_eq!(
            models[1].display.as_deref(),
            Some("Gemini 3 Pro"),
            "displayName is carried into the upstream row"
        );
    }

    /// A rejected catalogue is an upstream error the dashboard can show, and it never
    /// carries the access token in the message.
    #[tokio::test]
    async fn cca_fetch_reports_upstream_rejection() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1internal:fetchAvailableModels"))
            .respond_with(
                wiremock::ResponseTemplate::new(403)
                    .set_body_json(json!({"error": {"message": "denied"}})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let err = fetch_cca_models(
            &reqwest::Client::new(),
            &server.uri(),
            "at-secret",
            "proj-1",
        )
        .await
        .expect_err("rejected");
        assert!(
            matches!(err, Error::Upstream { status: 403, .. }),
            "{err:?}"
        );
        assert!(!err.to_string().contains("at-secret"), "{err}");
    }
}
