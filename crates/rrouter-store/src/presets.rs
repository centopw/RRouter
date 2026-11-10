//! Model presets: a named, revisioned layout of the model catalog.
//!
//! A preset is a saved view of the model list — which entries are shown, which are
//! inactive, and in what order. It is *not* a routing configuration: nothing here
//! changes how a request is dispatched. The layout is a document:
//!
//! ```json
//! {"schema_version":1,"auto_include_new":true,"items":[
//!   {"provider":"openai","id":"gpt-4.1","position":0,"visible":true}
//! ]}
//! ```
//!
//! `auto_include_new` is the compatibility switch: with an empty `items` list and the
//! flag on (the seeded default preset), the client merges every model in the live
//! catalog, so a preset only has to name what it wants to deviate from. Positions are
//! dense from zero within each visibility group (`visible` and inactive are separate
//! orderings), and `(provider, id)` is unique across the whole document — see
//! [`LayoutDocument::validate`].
//!
//! # Revisions
//!
//! A layout write is optimistic: the caller sends the revision it read and the store
//! either lands a new revision or reports [`PresetUpdateOutcome::Conflict`] with the
//! current one. Every accepted layout is snapshotted into `model_preset_versions` in the
//! same transaction as the live row, and only the newest
//! [`MAX_PRESET_VERSIONS`] snapshots are kept — the history is an undo trail, not an
//! archive.
//!
//! # Scope
//!
//! The schema reserves `scope`/`owner_user_id`/`team_id` for future sharing; this release
//! creates global presets only (`scope = 'global'`, both owner columns NULL). Reads do not
//! filter by them yet, so adding filtering later needs no migration.
//!
//! # Defaults and per-user selection
//!
//! Exactly one preset is the instance default — the v30 seed, or whichever preset
//! [`Store::set_default_model_preset`] promotes. [`Store::active_model_preset`] resolves a
//! user's stored selection and falls back to that default when the user has none (or when
//! the selected preset no longer exists). A delete removes the preference rows that point
//! at it in the same transaction, so the fallback cannot dangle.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use rrouter_core::{Error, Result};

use crate::migrations;
use crate::store::Store;

/// The only layout document version this build writes or accepts.
pub const LAYOUT_SCHEMA_VERSION: u32 = 1;
/// Items one layout may hold. Larger than any catalog this deployment can render.
pub const MAX_LAYOUT_ITEMS: usize = 20_000;
/// Longest provider name in a layout item.
pub const MAX_LAYOUT_PROVIDER: usize = 64;
/// Longest model id in a layout item.
pub const MAX_LAYOUT_MODEL_ID: usize = 200;
/// Groups one layout may define, including explicit provider buckets.
pub const MAX_LAYOUT_GROUPS: usize = 500;
/// Longest group id.
pub const MAX_LAYOUT_GROUP_ID: usize = 64;
/// Longest group display name.
pub const MAX_LAYOUT_GROUP_NAME: usize = 80;
/// Longest preset name.
pub const MAX_PRESET_NAME: usize = 80;
/// Longest preset description, in characters.
pub const MAX_PRESET_DESCRIPTION: usize = 500;
/// Longest actor identity a mutator accepts.
pub const MAX_PRESET_ACTOR: usize = 200;
/// Layout snapshots kept per preset; older revisions are pruned on write.
pub const MAX_PRESET_VERSIONS: i64 = 20;
/// The seeded global default. A fresh v30 database always has this preset.
pub const DEFAULT_MODEL_PRESET_ID: Uuid = Uuid::from_u128(1);

fn default_true() -> bool {
    true
}

/// One model in a preset layout.
///
/// `position` orders the item inside its visibility group: visible items are numbered
/// `0..n` and inactive items are numbered separately `0..m`. `(provider, id)` identifies
/// the catalog entry and must be unique across the document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutItem {
    /// Provider slug, the same spelling the catalog uses (`openai`, `anthropic`, …).
    pub provider: String,
    /// Model id within that provider.
    pub id: String,
    /// Explicit group this item belongs to. Omitted means the implicit provider bucket
    /// (or an explicit group whose `provider` matches this item's provider).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Zero-based order inside this item's visibility group.
    pub position: u32,
    /// `false` means the model is present in the preset but inactive. Omitted means
    /// visible, which keeps a hand-written document readable.
    #[serde(default = "default_true")]
    pub visible: bool,
    /// Where the item sat in the visible group before it was hidden, so unhide can
    /// restore it instead of appending. Omitted for items that were never visible; when
    /// present it must be below [`MAX_LAYOUT_ITEMS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_visible_position: Option<u32>,
    /// Unix seconds when the item was hidden. Omitted when the item is visible (or was
    /// never hidden); the store does not require it and does not interpret it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_at: Option<i64>,
}

/// One named group on a preset board.
///
/// Groups are presentation, not routing. A group with `provider` set is the bucket for
/// that provider's unassigned models and must use the stable id `provider:<provider>`;
/// a group without one is a free-form group that can mix providers. Array order is the
/// display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutGroup {
    /// Stable id; `provider:<provider>` for a provider bucket, a client-chosen id for a
    /// free-form group.
    pub id: String,
    pub name: String,
    /// Provider whose unassigned models belong here. Omitted for a free-form group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// A model preset layout document, exactly as stored in `layout_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutDocument {
    /// Document format version; this build accepts [`LAYOUT_SCHEMA_VERSION`] only.
    pub schema_version: u32,
    /// Merge models the document does not name. The default preset relies on this to
    /// stay valid as the catalog grows.
    #[serde(default = "default_true")]
    pub auto_include_new: bool,
    /// Explicit groups, in display order. Provider buckets not named here are implicit
    /// and render after the explicit list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<LayoutGroup>,
    #[serde(default)]
    pub items: Vec<LayoutItem>,
}

impl Default for LayoutDocument {
    /// The seeded default: no explicit items, every catalog model included.
    fn default() -> Self {
        Self {
            schema_version: LAYOUT_SCHEMA_VERSION,
            auto_include_new: true,
            groups: Vec::new(),
            items: Vec::new(),
        }
    }
}

impl LayoutDocument {
    /// Reject a document the store will not persist.
    ///
    /// Rules, in the order they are checked:
    ///
    /// * `schema_version` is exactly 1;
    /// * at most [`MAX_LAYOUT_ITEMS`] items;
    /// * every `provider`/`id` is non-empty after trimming and within its character bound;
    /// * no `(provider, id)` pair appears twice, in the same or different visibility group;
    /// * a remembered `last_visible_position` is below [`MAX_LAYOUT_ITEMS`];
    /// * positions are dense and unique from zero inside each visibility group.
    ///
    /// `last_visible_position` and `hidden_at` are restore memory owned by the client;
    /// they are optional and accepted in either visibility group.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != LAYOUT_SCHEMA_VERSION {
            return Err(Error::BadRequest(format!(
                "layout schema_version must be {LAYOUT_SCHEMA_VERSION}"
            )));
        }
        if self.items.len() > MAX_LAYOUT_ITEMS {
            return Err(Error::BadRequest(format!(
                "a layout holds at most {MAX_LAYOUT_ITEMS} items"
            )));
        }
        if self.groups.len() > MAX_LAYOUT_GROUPS {
            return Err(Error::BadRequest(format!(
                "a layout holds at most {MAX_LAYOUT_GROUPS} groups"
            )));
        }

        let mut group_by_id: std::collections::HashMap<&str, &LayoutGroup> =
            std::collections::HashMap::with_capacity(self.groups.len());
        let mut providers = HashSet::with_capacity(self.groups.len());
        for group in &self.groups {
            let id = group.id.trim();
            let name = group.name.trim();
            if id.is_empty() {
                return Err(Error::BadRequest("a layout group needs an id".to_string()));
            }
            if id.chars().count() > MAX_LAYOUT_GROUP_ID {
                return Err(Error::BadRequest(format!(
                    "a layout group id is limited to {MAX_LAYOUT_GROUP_ID} characters"
                )));
            }
            if name.is_empty() {
                return Err(Error::BadRequest("a layout group needs a name".to_string()));
            }
            if name.chars().count() > MAX_LAYOUT_GROUP_NAME {
                return Err(Error::BadRequest(format!(
                    "a layout group name is limited to {MAX_LAYOUT_GROUP_NAME} characters"
                )));
            }
            if group_by_id.insert(id, group).is_some() {
                return Err(Error::BadRequest(format!(
                    "duplicate layout group id: {id}"
                )));
            }
            match group.provider.as_deref().map(str::trim) {
                Some("") => {
                    return Err(Error::BadRequest(
                        "a provider group needs a provider".to_string(),
                    ));
                }
                Some(provider) => {
                    if provider.chars().count() > MAX_LAYOUT_PROVIDER {
                        return Err(Error::BadRequest(format!(
                            "a layout group provider is limited to {MAX_LAYOUT_PROVIDER} characters"
                        )));
                    }
                    if id != format!("provider:{provider}") {
                        return Err(Error::BadRequest(format!(
                            "provider group {id} must use the id provider:{provider}"
                        )));
                    }
                    if !providers.insert(provider) {
                        return Err(Error::BadRequest(format!(
                            "duplicate provider group for {provider}"
                        )));
                    }
                }
                None if id.starts_with("provider:") => {
                    return Err(Error::BadRequest(format!(
                        "group id {id} uses the reserved provider: prefix"
                    )));
                }
                None => {}
            }
        }

        let mut seen: HashSet<(&str, &str)> = HashSet::with_capacity(self.items.len());
        let mut visible = Vec::new();
        let mut inactive = Vec::new();
        for item in &self.items {
            let provider = item.provider.trim();
            let id = item.id.trim();
            if provider.is_empty() {
                return Err(Error::BadRequest(
                    "a layout item needs a provider".to_string(),
                ));
            }
            if id.is_empty() {
                return Err(Error::BadRequest(
                    "a layout item needs a model id".to_string(),
                ));
            }
            if provider.chars().count() > MAX_LAYOUT_PROVIDER {
                return Err(Error::BadRequest(format!(
                    "a layout provider is limited to {MAX_LAYOUT_PROVIDER} characters"
                )));
            }
            if id.chars().count() > MAX_LAYOUT_MODEL_ID {
                return Err(Error::BadRequest(format!(
                    "a layout model id is limited to {MAX_LAYOUT_MODEL_ID} characters"
                )));
            }
            if !seen.insert((provider, id)) {
                return Err(Error::BadRequest(format!(
                    "duplicate layout item: {provider}/{id}"
                )));
            }
            if let Some(group) = item.group.as_deref() {
                let group = group.trim();
                let Some(target) = group_by_id.get(group) else {
                    return Err(Error::BadRequest(format!(
                        "layout item {provider}/{id} references unknown group {group}"
                    )));
                };
                if let Some(group_provider) = target.provider.as_deref() {
                    if group_provider.trim() != provider {
                        return Err(Error::BadRequest(format!(
                            "layout item {provider}/{id} cannot join provider group {group}"
                        )));
                    }
                }
            }
            if item
                .last_visible_position
                .is_some_and(|position| position as usize >= MAX_LAYOUT_ITEMS)
            {
                return Err(Error::BadRequest(format!(
                    "a last_visible_position must be below {MAX_LAYOUT_ITEMS}"
                )));
            }
            if item.visible {
                visible.push(item.position);
            } else {
                inactive.push(item.position);
            }
        }

        for (label, mut positions) in [("visible", visible), ("inactive", inactive)] {
            positions.sort_unstable();
            for (expected, actual) in positions.iter().enumerate() {
                if *actual as usize != expected {
                    return Err(Error::BadRequest(format!(
                        "{label} layout positions must be dense from 0: found {actual} at \
                         index {expected}"
                    )));
                }
            }
        }
        Ok(())
    }
}

/// One preset as the store reports it. `layout` is the parsed `layout_json` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPreset {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    /// `global` in this release; the column exists so sharing needs no migration.
    pub scope: String,
    pub owner_user_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub is_default: bool,
    /// Bumped only by a layout write; metadata edits leave it alone.
    pub revision: i64,
    pub layout: LayoutDocument,
    /// Who created the preset (or this revision, in [`ModelPresetVersion`]).
    pub created_by: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One snapshot of a preset's layout. Newest revision first from
/// [`Store::model_preset_versions`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPresetVersion {
    pub preset_id: Uuid,
    pub revision: i64,
    pub layout: LayoutDocument,
    pub created_by: Option<String>,
    pub created_at: i64,
}

/// Result of an optimistic layout write.
///
/// The updated variant carries the full preset because every caller needs it; boxing that
/// would add an allocation to the common success path for a short-lived local value.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PresetUpdateOutcome {
    /// The layout landed; the returned preset carries the new revision.
    Updated(ModelPreset),
    /// `expected_revision` was stale. The stored layout was not touched.
    Conflict { current_revision: i64 },
    /// No preset with that id.
    NotFound,
}

const PRESET_COLUMNS: &str = "id, name, description, scope, owner_user_id, team_id, is_default, \
     revision, layout_json, created_by, created_at, updated_at";

fn uuid_col(index: usize, raw: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn opt_uuid_col(index: usize, raw: Option<String>) -> rusqlite::Result<Option<Uuid>> {
    raw.map(|value| uuid_col(index, value)).transpose()
}

fn row_to_preset(r: &Row<'_>) -> rusqlite::Result<ModelPreset> {
    let raw_layout: String = r.get(8)?;
    let layout = serde_json::from_str(&raw_layout).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(ModelPreset {
        id: uuid_col(0, r.get(0)?)?,
        name: r.get(1)?,
        description: r.get(2)?,
        scope: r.get(3)?,
        owner_user_id: opt_uuid_col(4, r.get(4)?)?,
        team_id: opt_uuid_col(5, r.get(5)?)?,
        is_default: r.get::<_, i64>(6)? != 0,
        revision: r.get(7)?,
        layout,
        created_by: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
    })
}

fn row_to_version(r: &Row<'_>) -> rusqlite::Result<ModelPresetVersion> {
    let raw_layout: String = r.get(2)?;
    let layout = serde_json::from_str(&raw_layout).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(ModelPresetVersion {
        preset_id: uuid_col(0, r.get(0)?)?,
        revision: r.get(1)?,
        layout,
        created_by: r.get(3)?,
        created_at: r.get(4)?,
    })
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn check_name(raw: &str) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(Error::BadRequest("a preset name is required".into()));
    }
    if name.chars().count() > MAX_PRESET_NAME {
        return Err(Error::BadRequest(format!(
            "a preset name is limited to {MAX_PRESET_NAME} characters"
        )));
    }
    Ok(name.to_string())
}

fn check_description(raw: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let description = raw.trim();
    if description.is_empty() {
        return Ok(None);
    }
    if description.chars().count() > MAX_PRESET_DESCRIPTION {
        return Err(Error::BadRequest(format!(
            "a preset description is limited to {MAX_PRESET_DESCRIPTION} characters"
        )));
    }
    Ok(Some(description.to_string()))
}

/// Every mutator requires an actor even where the schema has no `updated_by` column, so a
/// caller cannot silently omit the identity that is recorded on creation and snapshots.
fn check_actor(raw: &str) -> Result<String> {
    let actor = raw.trim();
    if actor.is_empty() {
        return Err(Error::BadRequest("an actor is required".into()));
    }
    if actor.chars().count() > MAX_PRESET_ACTOR {
        return Err(Error::BadRequest(format!(
            "an actor is limited to {MAX_PRESET_ACTOR} characters"
        )));
    }
    Ok(actor.to_string())
}

/// Validate and serialize a layout for `layout_json`.
fn encode_layout(layout: &LayoutDocument) -> Result<String> {
    layout.validate()?;
    serde_json::to_string(layout)
        .map_err(|e| Error::Store(format!("serialize model preset layout: {e}")))
}

/// Drop every snapshot older than the newest [`MAX_PRESET_VERSIONS`] for one preset.
fn prune_versions(tx: &rusqlite::Transaction<'_>, preset_id: &Uuid, revision: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM model_preset_versions WHERE preset_id = ?1 AND revision <= ?2",
        params![preset_id.to_string(), revision - MAX_PRESET_VERSIONS],
    )
    .map_err(migrations::db_err)?;
    Ok(())
}

fn insert_version(
    tx: &rusqlite::Transaction<'_>,
    preset_id: &Uuid,
    revision: i64,
    layout_json: &str,
    actor: &str,
    stamp: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO model_preset_versions \
         (preset_id, revision, layout_json, created_by, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![preset_id.to_string(), revision, layout_json, actor, stamp],
    )
    .map_err(migrations::db_err)?;
    Ok(())
}

fn read_preset(tx: &rusqlite::Transaction<'_>, id: &Uuid) -> Result<Option<ModelPreset>> {
    tx.query_row(
        &format!("SELECT {PRESET_COLUMNS} FROM model_presets WHERE id = ?1"),
        params![id.to_string()],
        row_to_preset,
    )
    .optional()
    .map_err(migrations::db_err)
}

impl Store {
    // ----- reads -------------------------------------------------------------------

    /// Every preset, default first, then by name. The order is the dashboard list's.
    pub async fn list_model_presets(&self) -> Result<Vec<ModelPreset>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {PRESET_COLUMNS} FROM model_presets \
                 ORDER BY is_default DESC, name ASC, id ASC"
            ))
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map([], row_to_preset)
            .map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    pub async fn get_model_preset(&self, id: Uuid) -> Result<Option<ModelPreset>> {
        self.conn()
            .query_row(
                &format!("SELECT {PRESET_COLUMNS} FROM model_presets WHERE id = ?1"),
                params![id.to_string()],
                row_to_preset,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// The instance default, if the seed or a promotion established one.
    pub async fn default_model_preset(&self) -> Result<Option<ModelPreset>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {PRESET_COLUMNS} FROM model_presets WHERE is_default = 1 \
                     ORDER BY created_at ASC, id ASC LIMIT 1"
                ),
                [],
                row_to_preset,
            )
            .optional()
            .map_err(migrations::db_err)
    }

    /// The preset a user is looking at: their stored selection when it still resolves,
    /// otherwise the instance default. Always returns a preset in a migrated database;
    /// a missing default is a storage error rather than a silent empty view.
    pub async fn active_model_preset(&self, user: Option<Uuid>) -> Result<ModelPreset> {
        if let Some(user) = user {
            let found = self
                .conn()
                .query_row(
                    &format!(
                        "SELECT {PRESET_COLUMNS} FROM model_presets WHERE id = \
                         (SELECT active_preset_id FROM user_model_preferences WHERE user_id = ?1)"
                    ),
                    params![user.to_string()],
                    row_to_preset,
                )
                .optional()
                .map_err(migrations::db_err)?;
            if let Some(preset) = found {
                return Ok(preset);
            }
        }
        self.default_model_preset()
            .await?
            .ok_or_else(|| Error::Store("the default model preset is missing".into()))
    }

    /// Snapshot history for one preset, newest revision first. Empty for an unknown id.
    pub async fn model_preset_versions(&self, id: Uuid) -> Result<Vec<ModelPresetVersion>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT preset_id, revision, layout_json, created_by, created_at \
                 FROM model_preset_versions WHERE preset_id = ?1 ORDER BY revision DESC",
            )
            .map_err(migrations::db_err)?;
        let rows = stmt
            .query_map(params![id.to_string()], row_to_version)
            .map_err(migrations::db_err)?;
        rows.map(|r| r.map_err(migrations::db_err)).collect()
    }

    // ----- mutations ---------------------------------------------------------------

    /// Create a global preset at revision 1 and snapshot that revision.
    pub async fn create_model_preset(
        &self,
        name: &str,
        description: Option<&str>,
        layout: &LayoutDocument,
        actor: &str,
    ) -> Result<ModelPreset> {
        let name = check_name(name)?;
        let description = check_description(description)?;
        let actor = check_actor(actor)?;
        let layout_json = encode_layout(layout)?;
        let stamp = now();
        let preset = ModelPreset {
            id: Uuid::new_v4(),
            name,
            description,
            scope: "global".to_string(),
            owner_user_id: None,
            team_id: None,
            is_default: false,
            revision: 1,
            layout: layout.clone(),
            created_by: Some(actor.clone()),
            created_at: stamp,
            updated_at: stamp,
        };

        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        tx.execute(
            "INSERT INTO model_presets \
             (id, name, description, scope, owner_user_id, team_id, is_default, revision, \
              layout_json, created_by, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                preset.id.to_string(),
                preset.name,
                preset.description,
                preset.scope,
                preset.owner_user_id.map(|id| id.to_string()),
                preset.team_id.map(|id| id.to_string()),
                i64::from(preset.is_default),
                preset.revision,
                layout_json,
                preset.created_by,
                preset.created_at,
                preset.updated_at,
            ],
        )
        .map_err(migrations::db_err)?;
        insert_version(
            &tx,
            &preset.id,
            preset.revision,
            &layout_json,
            &actor,
            stamp,
        )?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(preset)
    }

    /// Rename and/or re-describe a preset. `None` leaves a field unchanged;
    /// `Some(None)` clears the description. Neither touches the layout or the revision.
    /// `Ok(None)` means no such id.
    pub async fn update_model_preset_meta(
        &self,
        id: Uuid,
        name: Option<&str>,
        description: Option<Option<&str>>,
        actor: &str,
    ) -> Result<Option<ModelPreset>> {
        check_actor(actor)?;
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let Some(mut preset) = read_preset(&tx, &id)? else {
            return Ok(None);
        };
        if let Some(name) = name {
            preset.name = check_name(name)?;
        }
        if let Some(description) = description {
            preset.description = check_description(description)?;
        }
        preset.updated_at = now();
        tx.execute(
            "UPDATE model_presets SET name = ?2, description = ?3, updated_at = ?4 WHERE id = ?1",
            params![
                id.to_string(),
                preset.name,
                preset.description,
                preset.updated_at
            ],
        )
        .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(Some(preset))
    }

    /// Write a layout only if the caller's revision is current.
    ///
    /// The update, the revision bump, the snapshot and the timestamp are one
    /// transaction, so a concurrent winner is never partially overwritten.
    pub async fn update_model_preset_layout(
        &self,
        id: Uuid,
        expected_revision: i64,
        layout: &LayoutDocument,
        actor: &str,
    ) -> Result<PresetUpdateOutcome> {
        let actor = check_actor(actor)?;
        let layout_json = encode_layout(layout)?;
        let stamp = now();
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let Some(current) = read_preset(&tx, &id)? else {
            return Ok(PresetUpdateOutcome::NotFound);
        };
        if current.revision != expected_revision {
            return Ok(PresetUpdateOutcome::Conflict {
                current_revision: current.revision,
            });
        }
        let revision = current.revision + 1;
        tx.execute(
            "UPDATE model_presets SET layout_json = ?2, revision = ?3, updated_at = ?4 \
             WHERE id = ?1",
            params![id.to_string(), layout_json, revision, stamp],
        )
        .map_err(migrations::db_err)?;
        insert_version(&tx, &id, revision, &layout_json, &actor, stamp)?;
        prune_versions(&tx, &id, revision)?;
        let updated = read_preset(&tx, &id)?
            .ok_or_else(|| Error::Store(format!("model preset {id} disappeared during update")))?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(PresetUpdateOutcome::Updated(updated))
    }

    /// Delete a preset, its snapshots and every user selection that pointed at it, in one
    /// transaction. The default preset must be demoted first, so a delete can never leave
    /// the instance without a fallback. `false` means no such id.
    pub async fn delete_model_preset(&self, id: Uuid, actor: &str) -> Result<bool> {
        check_actor(actor)?;
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let is_default: Option<i64> = tx
            .query_row(
                "SELECT is_default FROM model_presets WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(migrations::db_err)?;
        let Some(is_default) = is_default else {
            return Ok(false);
        };
        if is_default != 0 {
            return Err(Error::BadRequest(
                "the default model preset cannot be deleted; make another preset the \
                 default first"
                    .into(),
            ));
        }
        let sid = id.to_string();
        tx.execute(
            "DELETE FROM model_preset_versions WHERE preset_id = ?1",
            params![sid],
        )
        .map_err(migrations::db_err)?;
        tx.execute(
            "DELETE FROM user_model_preferences WHERE active_preset_id = ?1",
            params![sid],
        )
        .map_err(migrations::db_err)?;
        tx.execute("DELETE FROM model_presets WHERE id = ?1", params![sid])
            .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(true)
    }

    /// Copy a preset's layout into a new preset at revision 1. The caller names the copy;
    /// names are not unique, so a caller that wants "Copy of X" can have it.
    pub async fn duplicate_model_preset(
        &self,
        id: Uuid,
        name: &str,
        actor: &str,
    ) -> Result<ModelPreset> {
        let name = check_name(name)?;
        let actor = check_actor(actor)?;
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let Some(source) = read_preset(&tx, &id)? else {
            return Err(Error::NotFound(format!("model preset {id}")));
        };
        let layout_json = serde_json::to_string(&source.layout)
            .map_err(|e| Error::Store(format!("serialize model preset layout: {e}")))?;
        let stamp = now();
        let copy = ModelPreset {
            id: Uuid::new_v4(),
            name,
            description: source.description,
            scope: source.scope,
            owner_user_id: source.owner_user_id,
            team_id: source.team_id,
            is_default: false,
            revision: 1,
            layout: source.layout,
            created_by: Some(actor.clone()),
            created_at: stamp,
            updated_at: stamp,
        };
        tx.execute(
            "INSERT INTO model_presets \
             (id, name, description, scope, owner_user_id, team_id, is_default, revision, \
              layout_json, created_by, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 1, ?7, ?8, ?9, ?9)",
            params![
                copy.id.to_string(),
                copy.name,
                copy.description,
                copy.scope,
                copy.owner_user_id.map(|id| id.to_string()),
                copy.team_id.map(|id| id.to_string()),
                layout_json,
                copy.created_by,
                copy.created_at,
            ],
        )
        .map_err(migrations::db_err)?;
        insert_version(&tx, &copy.id, 1, &layout_json, &actor, stamp)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(copy)
    }

    /// Promote one preset and demote the previous default atomically. Idempotent.
    pub async fn set_default_model_preset(&self, id: Uuid, actor: &str) -> Result<ModelPreset> {
        check_actor(actor)?;
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        if read_preset(&tx, &id)?.is_none() {
            return Err(Error::NotFound(format!("model preset {id}")));
        }
        let stamp = now();
        let sid = id.to_string();
        tx.execute(
            "UPDATE model_presets SET is_default = 0 WHERE is_default <> 0 AND id <> ?1",
            params![sid],
        )
        .map_err(migrations::db_err)?;
        tx.execute(
            "UPDATE model_presets SET is_default = 1, updated_at = ?2 WHERE id = ?1",
            params![sid, stamp],
        )
        .map_err(migrations::db_err)?;
        let promoted = read_preset(&tx, &id)?
            .ok_or_else(|| Error::Store(format!("model preset {id} disappeared during update")))?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(promoted)
    }

    /// Remember which preset one user is viewing. The preset must exist; the user id is
    /// stored as given (the store does not own identity).
    pub async fn set_active_model_preset(&self, user: Uuid, id: Uuid) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(migrations::db_err)?;
        let exists: Option<String> = tx
            .query_row(
                "SELECT id FROM model_presets WHERE id = ?1",
                params![id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(migrations::db_err)?;
        if exists.is_none() {
            return Err(Error::NotFound(format!("model preset {id}")));
        }
        tx.execute(
            "INSERT INTO user_model_preferences (user_id, active_preset_id, updated_at) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(user_id) DO UPDATE SET \
             active_preset_id = excluded.active_preset_id, updated_at = excluded.updated_at",
            params![user.to_string(), id.to_string(), now()],
        )
        .map_err(migrations::db_err)?;
        tx.commit().map_err(migrations::db_err)?;
        Ok(())
    }
}
