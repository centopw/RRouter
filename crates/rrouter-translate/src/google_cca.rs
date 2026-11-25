//! Google Code Assist (Cloud Code Assist): the upstream wire the Antigravity desktop
//! client speaks.
//!
//! This is a **provider dialect**, not an inbound one. A client still talks OpenAI Chat,
//! Anthropic Messages or Responses; the gateway translates that canonical request into
//! the envelope below and translates the answer back. [`Dialect::GoogleCodeAssist`] exists
//! so the proxy can name the upstream grammar without exposing a route.
//!
//! ```json
//! {
//!   "model": "gemini-3-pro",
//!   "userAgent": "antigravity",
//!   "requestType": "agent",
//!   "project": "<cloud project id>",
//!   "requestId": "agent-<uuid>",
//!   "request": {
//!     "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
//!     "systemInstruction": {"parts": [{"text": "…"}]},
//!     "generationConfig": {"maxOutputTokens": 4096}
//!   }
//! }
//! ```
//!
//! `request` deliberately carries no `model` and no `safetySettings`: neither belongs in
//! the inner Gemini request, and the Antigravity backend rejects a nested model.
//!
//! # `sessionId`
//!
//! Cloud Code Assist used to accept an optional top-level `sessionId`, a locally derived
//! conversation handle that nothing in the canonical request carries and nothing on the
//! response side reads. Older builds sent one; cloudcode-pa no longer declares the member,
//! so every model now answers
//!
//! ```json
//! {"error": {"code": 400, "status": "INVALID_ARGUMENT",
//!   "message": "Invalid JSON payload received. Unknown name \"sessionId\": Cannot find field."}}
//! ```
//!
//! The envelope therefore carries **no** `sessionId`: it is neither built, derived nor
//! repaired back in. Asserting its absence in [`tests::builds_the_antigravity_envelope`] is
//! the regression guard — a member this builder never knew as valid may not reappear
//! without an upstream that declares it.
//!
//! # Envelope repair
//!
//! The envelope has no optional member today, so [`rejected_fields`] is always empty and
//! [`without_fields`] removes nothing: the gateway still asks the question, and the answer
//! is "nothing may be dropped". The machinery stays because it is the only path that may
//! ever lose a member whose absence still leaves a valid request, and it is gated on
//! [`DROPPABLE`] rather than on what an upstream happens to name.
//!
//! Identity is *not* repairable. `model`, `userAgent`, `requestType`, `project` and
//! `requestId` are what makes the call this account's call; an envelope without them still
//! parses, so nothing fails loudly while every later request runs under a partial identity.
//! An upstream answering `Unknown name "userAgent"` is saying the envelope shape is wrong
//! for this account or model version — not that `userAgent` is optional. Those names are
//! kept in the [`REQUIRED`] bucket: [`EnvelopeRejection`] reports them as
//! [`UnrepairableEnvelope`] so the caller surfaces them, and [`rejected_fields`] — the
//! repair-only view — reports nothing for such an answer.
//!
//! # Account refusals
//!
//! A refusal is not always about the request. Google answers a terminated account with
//! `403` `{"error": {"status": "TOS_VIOLATION", "message": "The service has been disabled
//! in this account for violation of our Terms of Service."}}` and a not-yet-verified one
//! with `VALIDATION_REQUIRED` / "Verify your account to continue" — which arrives on
//! healthy, fully-quota'd accounts. [`refusal`] separates those from ordinary failures so
//! the gateway can retire a permanently banned account instead of retrying it forever,
//! and cool a verification-required one down without retiring it.
//!
//! # Responses
//!
//! Every buffered body and every SSE `data:` payload wraps the Gemini response:
//! `{"response": {"candidates": …, "usageMetadata": …}, "traceId": "…"}`. Both readers
//! unwrap `response` and also accept a bare Gemini body, so a mock or a future server that
//! drops the wrapper still works.
//!
//! # Streams
//!
//! `streamGenerateContent?alt=sse` emits `data: {json}` frames with no `event:` name.
//! Text, reasoning (`thought: true`), function calls and usage all arrive incrementally;
//! the decoder folds them into the canonical [`StreamEvent`] vocabulary.

use std::sync::OnceLock;

use serde_json::{json, Map, Value};
use uuid::Uuid;

use rrouter_core::sse::{SseCodec, SseFrame};
use rrouter_core::{
    ChatRequest, ChatResponse, Error, FinishReason, Message, Part, Refusal, Result, Role,
    StreamEvent, ToolChoice, Usage,
};

/// Daily build host. Antigravity desktop points here first.
pub const DAILY_HOST: &str = "https://daily-cloudcode-pa.googleapis.com";
/// Production fallback host, used when the daily build transport-fails or answers 5xx.
pub const PROD_HOST: &str = "https://cloudcode-pa.googleapis.com";
/// Streaming path, with the SSE selector the endpoint requires.
pub const STREAM_PATH: &str = "/v1internal:streamGenerateContent?alt=sse";
/// Buffered path.
pub const NON_STREAM_PATH: &str = "/v1internal:generateContent";
/// Product, build string and client id of the wire `User-Agent`. The backend fingerprints
/// the whole marker, so these stay fixed rather than following this gateway's own version.
/// Only the host tokens in the marker ([`client_identity`]) describe the machine.
const USER_AGENT_PRODUCT: &str = "antigravity/hub/2.26.0";
/// `cl=` component of the marker: the desktop client's build counter.
const USER_AGENT_CLIENT_ID: &str = "1005016104";

/// The host the Antigravity client declares, as both upstream surfaces observe it.
///
/// The backend sees one machine through two different fields: the wire `User-Agent`
/// carries the `(aidev_client; os_type=…; arch=…; cl=…)` marker, and
/// `v1internal:loadCodeAssist` / `v1internal:onboardUser` carry `metadata.platform`, a
/// `ClientMetadata.Platform` enum value. Deriving them from separate places let a single
/// client announce `os_type=darwin; arch=arm64` on the wire while telling project
/// discovery `platform: 3` (Linux x64) — two layers of one client disagreeing about the
/// machine, which is a stronger abuse signal than either value alone.
///
/// [`client_identity`] is the single source for both: the marker tokens below and
/// [`Self::platform`] are resolved together from the same host pair, so they cannot
/// disagree. Do not derive either value anywhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIdentity {
    os_type: &'static str,
    arch: &'static str,
    platform: i32,
}

impl ClientIdentity {
    /// `os_type=` token of the wire marker: `darwin`, `linux` or `windows`.
    pub const fn os_type(&self) -> &'static str {
        self.os_type
    }

    /// `arch=` token of the wire marker: `x86_64`, `arm64` or `aarch64`.
    pub const fn arch(&self) -> &'static str {
        self.arch
    }

    /// The `ClientMetadata.Platform` value naming the same host as [`Self::os_type`] and
    /// [`Self::arch`]: 1 macOS x64, 2 macOS arm64, 3 Linux x64, 4 Linux arm64, 5 Windows.
    pub const fn platform(&self) -> i32 {
        self.platform
    }

    /// The wire `User-Agent` for this identity. Product and client id never vary; the
    /// `os_type=` and `arch=` tokens are the host [`client_identity`] resolved.
    pub fn user_agent(&self) -> String {
        format!(
            "{USER_AGENT_PRODUCT} (aidev_client; os_type={}; arch={}; cl={USER_AGENT_CLIENT_ID})",
            self.os_type, self.arch
        )
    }
}

/// Identity of the host this build runs on: one read of `std::env::consts` feeding both
/// the wire marker and the platform enum.
pub fn client_identity() -> ClientIdentity {
    identity_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// [`client_identity`]'s wire `User-Agent`, resolved once per process.
///
/// The string is sent on every CCA call and every catalogue/discovery request, and it is
/// a pure function of the host, so it is cached. This replaces the former `USER_AGENT`
/// constant, which hardcoded the macOS arm64 host and therefore contradicted
/// [`ClientIdentity::platform`] on every other machine.
pub fn user_agent() -> &'static str {
    static WIRE: OnceLock<String> = OnceLock::new();
    WIRE.get_or_init(|| client_identity().user_agent())
}

/// The fallback identity for a host `ClientMetadata.Platform` cannot name: Linux x64.
///
/// One documented constant, not a second rule: an unrecognized host (FreeBSD, a 32-bit
/// ARM Linux, an architecture the enum has no member for) declares the Linux x64 identity
/// on **both** layers, so it can never end up with a marker and an enum that disagree.
/// The old `_ => 0` answered a value that is not a member of the enum at all.
const FALLBACK_IDENTITY: ClientIdentity = ClientIdentity {
    os_type: "linux",
    arch: "x86_64",
    platform: 3,
};

/// The one table both layers read: host `(OS, ARCH)` → marker tokens and platform code.
///
/// Every row names a real `ClientMetadata.Platform` member, so no supported host declares
/// `0`. macOS keeps the desktop client's own `darwin`/`arm64` vocabulary because that
/// spelling is the one upstream is known to accept; the other rows spell the Rust host
/// names (`std::env::consts::OS`/`ARCH`), which is the value the enum is derived from.
/// Windows has a single enum member and no arch distinction there, so every Windows host
/// declares the x64 marker with it.
fn identity_for(os: &'static str, arch: &'static str) -> ClientIdentity {
    match (os, arch) {
        ("macos", "x86_64") => ClientIdentity {
            os_type: "darwin",
            arch: "x86_64",
            platform: 1,
        },
        ("macos", "aarch64") => ClientIdentity {
            os_type: "darwin",
            arch: "arm64",
            platform: 2,
        },
        ("linux", "x86_64") => ClientIdentity {
            os_type: "linux",
            arch: "x86_64",
            platform: 3,
        },
        ("linux", "aarch64") => ClientIdentity {
            os_type: "linux",
            arch: "aarch64",
            platform: 4,
        },
        ("windows", _) => ClientIdentity {
            os_type: "windows",
            arch: "x86_64",
            platform: 5,
        },
        _ => FALLBACK_IDENTITY,
    }
}

/// `userAgent` inside the envelope. The desktop client sends the bare product name here
/// and the long fingerprint only on the wire; the two are not the same string.
pub const ENVELOPE_USER_AGENT: &str = "antigravity";
/// `X-Goog-Api-Client` value the Antigravity desktop sends.
pub const CLIENT_HEADER: &str = "google-cloud-sdk vscode_cloudshelleditor/0.1";
/// Env override for the daily host (a deployment pin or a test server).
pub const ENV_DAILY_HOST: &str = "RROUTER_ANTIGRAVITY_DAILY_HOST";
/// Env override for the production fallback host.
pub const ENV_PROD_HOST: &str = "RROUTER_ANTIGRAVITY_PROD_HOST";

/// Daily host: the env override wins, then the provider's configured base URL.
pub fn daily_host(configured_base: &str) -> String {
    env_host(ENV_DAILY_HOST).unwrap_or_else(|| configured_base.trim_end_matches('/').to_string())
}

/// Production fallback host: the env override wins, else [`PROD_HOST`].
pub fn prod_host() -> String {
    env_host(ENV_PROD_HOST).unwrap_or_else(|| PROD_HOST.to_string())
}

fn env_host(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

/// Full URL for one CCA call. `base` is an origin, not a path.
pub fn endpoint(base: &str, stream: bool) -> String {
    let mut url = String::with_capacity(base.trim_end_matches('/').len() + NON_STREAM_PATH.len());
    url.push_str(base.trim_end_matches('/'));
    url.push_str(if stream { STREAM_PATH } else { NON_STREAM_PATH });
    url
}

/// Build one envelope with a fresh `agent-<uuid>` request id.
///
/// `model` is the resolved model id (the candidate's), and `project` is the credential's
/// `project_id`. An empty project is a client-visible configuration error, not a panic.
pub fn build_request(req: &ChatRequest, model: &str, project: &str) -> Result<Value> {
    let request_id = format!("agent-{}", Uuid::new_v4());
    build_request_with(req, model, project, &request_id)
}

/// [`build_request`] with a caller-supplied request id, for retries that must stay
/// idempotent and for deterministic tests.
pub fn build_request_with(
    req: &ChatRequest,
    model: &str,
    project: &str,
    request_id: &str,
) -> Result<Value> {
    let project = project.trim();
    if project.is_empty() {
        return Err(Error::BadRequest(
            "a google-antigravity connection needs credentials.extra.project_id".into(),
        ));
    }
    if model.trim().is_empty() {
        return Err(Error::BadRequest("a model id is required".into()));
    }

    let mut system_parts: Vec<Value> = Vec::new();
    if let Some(system) = req.system.as_deref().filter(|s| !s.is_empty()) {
        system_parts.push(json!({ "text": system }));
    }
    let contents = contents_in(&req.messages, &mut system_parts)?;

    let mut request = Map::new();
    if !contents.is_empty() {
        request.insert("contents".into(), Value::Array(contents));
    }
    if !system_parts.is_empty() {
        request.insert("systemInstruction".into(), json!({ "parts": system_parts }));
    }
    let generation = generation_config(req);
    if !generation.is_empty() {
        request.insert("generationConfig".into(), Value::Object(generation));
    }
    if !req.tools.is_empty() {
        let declarations: Vec<Value> = req
            .tools
            .iter()
            .map(|tool| {
                let mut declaration = Map::new();
                declaration.insert("name".into(), Value::from(tool.name.clone()));
                if let Some(description) = tool.description.as_deref().filter(|d| !d.is_empty()) {
                    declaration.insert("description".into(), Value::from(description.to_string()));
                }
                let mut parameters = if tool.parameters.is_null() {
                    json!({"type": "object", "properties": {}})
                } else {
                    tool.parameters.clone()
                };
                gemini_schema(&mut parameters);
                declaration.insert("parameters".into(), parameters);
                Value::Object(declaration)
            })
            .collect();
        request.insert(
            "tools".into(),
            json!([{"functionDeclarations": declarations}]),
        );
    }
    if let Some(choice) = &req.tool_choice {
        let mut config = Map::new();
        match choice {
            ToolChoice::Auto => {
                config.insert("mode".into(), Value::from("AUTO"));
            }
            ToolChoice::None => {
                config.insert("mode".into(), Value::from("NONE"));
            }
            ToolChoice::Required => {
                config.insert("mode".into(), Value::from("ANY"));
            }
            ToolChoice::Tool(name) => {
                config.insert("mode".into(), Value::from("ANY"));
                config.insert(
                    "allowedFunctionNames".into(),
                    Value::Array(vec![Value::from(name.clone())]),
                );
            }
        }
        request.insert(
            "toolConfig".into(),
            json!({"functionCallingConfig": Value::Object(config)}),
        );
    }

    let mut envelope = Map::new();
    envelope.insert("model".into(), Value::from(model.to_string()));
    envelope.insert("userAgent".into(), Value::from(ENVELOPE_USER_AGENT));
    envelope.insert("requestType".into(), Value::from("agent"));
    envelope.insert("project".into(), Value::from(project.to_string()));
    envelope.insert("requestId".into(), Value::from(request_id.to_string()));
    envelope.insert("request".into(), Value::Object(request));
    Ok(Value::Object(envelope))
}

fn generation_config(req: &ChatRequest) -> Map<String, Value> {
    let mut config = Map::new();
    if let Some(max_tokens) = req.max_tokens {
        config.insert("maxOutputTokens".into(), Value::from(max_tokens));
    }
    if let Some(temperature) = req.temperature {
        config.insert("temperature".into(), Value::from(temperature));
    }
    if let Some(top_p) = req.top_p {
        config.insert("topP".into(), Value::from(top_p));
    }
    if !req.stop.is_empty() {
        config.insert("stopSequences".into(), json!(req.stop));
    }
    if let Some(reasoning) = &req.reasoning {
        config.insert(
            "thinkingConfig".into(),
            super::reasoning::google_thinking(reasoning, &req.model),
        );
    }
    config
}

/// Adapts JSON Schema type unions into Gemini `Schema` proto compatible fields.
///
/// Upstream Google Code Assist / Antigravity returns 400 INVALID_ARGUMENT (`Proto field is not
/// repeating, cannot start list`) when a schema node uses a JSON Schema type union array like
/// `{"type": ["number", "null"]}` because the proto `Type` enum field is not repeated.
/// This normalizes type arrays into a single `type` string with `nullable: true` or an `anyOf`
/// alternative list.
fn gemini_schema(schema: &mut Value) {
    let Value::Object(map) = schema else {
        return;
    };

    let type_rewrite = if let Some(Value::Array(arr)) = map.get("type") {
        let is_nullable = arr.iter().any(|v| v.as_str() == Some("null"));
        let mut non_null = Vec::new();
        for v in arr {
            if v.as_str() != Some("null") && !non_null.contains(v) {
                non_null.push(v.clone());
            }
        }
        Some((is_nullable, non_null))
    } else {
        None
    };

    if let Some((is_nullable, mut non_null)) = type_rewrite {
        match non_null.len() {
            0 => {
                map.insert("type".into(), Value::String("null".into()));
            }
            1 => {
                map.insert("type".into(), non_null.remove(0));
            }
            _ => {
                if map.contains_key("anyOf") {
                    map.insert("type".into(), non_null.remove(0));
                } else {
                    map.remove("type");
                    let any_of = non_null.into_iter().map(|t| json!({ "type": t })).collect();
                    map.insert("anyOf".into(), Value::Array(any_of));
                }
            }
        }

        if is_nullable {
            map.insert("nullable".into(), Value::Bool(true));
        }
    }

    if let Some(Value::Object(props)) = map.get_mut("properties") {
        for s in props.values_mut() {
            gemini_schema(s);
        }
    }
    if let Some(Value::Object(props)) = map.get_mut("patternProperties") {
        for s in props.values_mut() {
            gemini_schema(s);
        }
    }
    if let Some(Value::Object(defs)) = map.get_mut("$defs") {
        for s in defs.values_mut() {
            gemini_schema(s);
        }
    }
    if let Some(Value::Object(defs)) = map.get_mut("definitions") {
        for s in defs.values_mut() {
            gemini_schema(s);
        }
    }
    for kw in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(Value::Array(items)) = map.get_mut(kw) {
            for s in items {
                gemini_schema(s);
            }
        }
    }
    if let Some(items) = map.get_mut("items") {
        match items {
            Value::Object(_) => gemini_schema(items),
            Value::Array(arr) => {
                for s in arr {
                    gemini_schema(s);
                }
            }
            _ => {}
        }
    }
    if let Some(add_props) = map.get_mut("additionalProperties") {
        if add_props.is_object() {
            gemini_schema(add_props);
        }
    }
    if let Some(not_schema) = map.get_mut("not") {
        if not_schema.is_object() {
            gemini_schema(not_schema);
        }
    }
}

// ---------------------------------------------------------------------------
// Envelope repair
// ---------------------------------------------------------------------------

/// Top-level envelope keys a rejected-field repair may drop, and the whole of that set.
///
/// The repair is subtractive and remembered per account: whatever is dropped here is absent
/// from every later request until the process restarts, with no re-add and no alert. A name
/// may only be listed when the envelope without it is still a valid request — which is why
/// the set is **empty**. `sessionId` used to be the sole member; cloudcode-pa now rejects
/// it with `Unknown name`, so a build that still sent it would pay a rejected round trip on
/// every account, and listing it here would only repair accounts one probe at a time. The
/// envelope no longer sends it (see the module docs), and the empty table means the repair
/// path can never re-introduce or silently strip anything for an upstream that says so.
///
/// Identity keys are in [`REQUIRED`] and are never droppable. `request` is deliberately
/// absent from both: it carries the translated conversation, so dropping a member of it
/// would answer a different question instead of the same one in an older envelope shape.
const DROPPABLE: [&str; 0] = [];

/// Top-level keys that make the envelope a valid request for this account.
///
/// A `400` naming one of these is not a repair instruction. Dropping it would still produce
/// an envelope the upstream parses, so the account would keep serving traffic under a
/// partial identity — the failure this list exists to prevent. The correct reading is that
/// the envelope shape is wrong for this account or model version, which is what
/// [`EnvelopeRejection::error`] reports.
const REQUIRED: [&str; 5] = ["model", "userAgent", "requestType", "project", "requestId"];

/// One `400 INVALID_ARGUMENT`'s verdict on the envelope, split by what repair may do.
///
/// Google's protobuf parser answers an undocumented envelope member with
/// `{"error": {"status": "INVALID_ARGUMENT", "message": "Invalid JSON payload received.
/// Unknown name \"sessionId\": Cannot find field.", "details": [{"@type":
/// "type.googleapis.com/google.rpc.BadRequest", "fieldViolations": [{"field": "sessionId",
/// …}]}]}}`. Both carriers are read because the violation path is sometimes the enclosing
/// object rather than the offending name.
///
/// Two buckets, because the two answers call for opposite actions:
///
/// * [`droppable`](Self::droppable) — optional members the upstream does not know. Removing
///   them yields a valid request, so the resend drops them ([`without_fields`]). Nothing is
///   in that class today ([`DROPPABLE`] is empty), so the bucket stays empty and every
///   named field the envelope knows lands in `required`.
/// * [`required`](Self::required) — identity members the upstream does not know. Removing
///   them yields a request that still parses but is no longer this account's request, so
///   nothing in this bucket is ever dropped: [`fields_to_drop`](Self::fields_to_drop) is
///   empty whenever the bucket is non-empty, and [`error`](Self::error) hands the caller an
///   [`UnrepairableEnvelope`].
///
/// A mixed answer is fatal as a whole: dropping the optional member and resending would
/// retry the same broken identity, so `required` wins.
///
/// Every name under `request` is ignored, in both buckets: a malformed or unrelated `400`
/// can never make the caller strip part of the conversation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EnvelopeRejection {
    droppable: Vec<&'static str>,
    required: Vec<&'static str>,
}

impl EnvelopeRejection {
    /// Read a `400` body. A body that is not JSON, carries no `error`, or is not an
    /// `INVALID_ARGUMENT`/`BadRequest` names nothing at all.
    pub fn parse(body: &[u8]) -> Self {
        let mut rejection = Self::default();
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return rejection;
        };
        let Some(error) = value.get("error") else {
            return rejection;
        };
        let details = error
            .get("details")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let bad_request = error.get("status").and_then(Value::as_str) == Some("INVALID_ARGUMENT")
            || details.iter().any(|detail| {
                detail
                    .get("@type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.ends_with("BadRequest"))
            });
        if !bad_request {
            return rejection;
        }
        for detail in details {
            let violations = detail
                .get("fieldViolations")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for violation in violations {
                if let Some(field) = violation.get("field").and_then(Value::as_str) {
                    named_field(&mut rejection, field);
                }
                if let Some(text) = violation.get("description").and_then(Value::as_str) {
                    named_in_text(&mut rejection, text);
                }
            }
        }
        if let Some(message) = error.get("message").and_then(Value::as_str) {
            named_in_text(&mut rejection, message);
        }
        rejection
    }

    /// Optional members the upstream refused: exactly what the resend may omit.
    pub fn droppable(&self) -> &[&'static str] {
        &self.droppable
    }

    /// Identity members the upstream refused. Non-empty means "do not repair".
    pub fn required(&self) -> &[&'static str] {
        &self.required
    }

    /// The fields a resend may omit. Empty when the answer named an identity member, or
    /// named nothing this envelope may drop.
    pub fn fields_to_drop(&self) -> &[&'static str] {
        if self.required.is_empty() {
            &self.droppable
        } else {
            &[]
        }
    }

    /// Whether the body named no member this builder knows about.
    pub fn is_empty(&self) -> bool {
        self.droppable.is_empty() && self.required.is_empty()
    }

    /// The typed failure for an answer that named an identity member, or `None` when the
    /// answer is repairable (or says nothing).
    pub fn error(&self) -> Option<UnrepairableEnvelope> {
        (!self.required.is_empty()).then(|| UnrepairableEnvelope {
            fields: self.required.clone(),
        })
    }

    /// File one name in the bucket that decides what happens to it next.
    fn note(&mut self, name: &str) {
        if let Some(known) = optional_key(name) {
            push_unique(&mut self.droppable, known);
        } else if let Some(known) = required_key(name) {
            push_unique(&mut self.required, known);
        }
    }
}

/// A `400` that named an identity field: the envelope is wrong for this account.
///
/// This is deliberately not a repair instruction and deliberately not a drop. The caller
/// must not resend the same envelope with the field removed, because that partial envelope
/// is accepted: the account would keep serving traffic that no longer identifies it, which
/// is worse than a visible failure. Surface it — it is the upstream's own answer with the
/// fields named — and treat the account/model pair as misconfigured until the envelope for
/// it is fixed. [`EnvelopeRejection::error`] is the only constructor, so nothing can build
/// one for a repairable answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnrepairableEnvelope {
    fields: Vec<&'static str>,
}

impl UnrepairableEnvelope {
    /// The identity fields the upstream refused, in the order the body named them.
    pub fn fields(&self) -> &[&'static str] {
        &self.fields
    }

    /// The upstream's answer, kept as a pass-through `400` so clients see it exactly as the
    /// gateway already reports an unrepairable `400` — nothing is retried and nothing is
    /// rewritten.
    pub fn into_error(self) -> Error {
        Error::Upstream {
            provider: "google-antigravity".into(),
            status: 400,
            body: self.to_string(),
            payload: None,
        }
    }
}

impl std::fmt::Display for UnrepairableEnvelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "google code assist rejected required envelope field(s) `{}`: the envelope shape \
             is wrong for this account or model version, and repair never drops identity \
             fields",
            self.fields.join("`, `")
        )
    }
}

impl std::error::Error for UnrepairableEnvelope {}

/// Top-level envelope fields an upstream rejected **and repair may remove**, in the order
/// the body named them.
///
/// This is the repair-only view of [`EnvelopeRejection`]: identity members (`model`,
/// `userAgent`, `requestType`, `project`, `requestId`) are reported as empty here and show
/// up in [`EnvelopeRejection::required`] instead, so a caller that only asks "what may I
/// drop?" can never strip one. Nothing under `request` is ever reported.
pub fn rejected_fields(body: &[u8]) -> Vec<&'static str> {
    EnvelopeRejection::parse(body).fields_to_drop().to_vec()
}

/// The envelope without `fields`, so a resend omits exactly what the upstream refused.
///
/// A non-object passes through untouched: there is nothing to repair. Names outside
/// [`DROPPABLE`] are ignored even when a caller passes them, so the subtractive path has a
/// second gate: a stale list (the gateway remembers repairs per account in memory) or a
/// future caller's mistake cannot remove `model`/`userAgent`/`requestType`/`project`/
/// `requestId`, and `request` itself can never be removed.
pub fn without_fields(envelope: Value, fields: &[&str]) -> Value {
    let Value::Object(mut envelope) = envelope else {
        return envelope;
    };
    for field in fields {
        if DROPPABLE.contains(field) {
            envelope.remove(*field);
        }
    }
    Value::Object(envelope)
}

/// Note `field` when it names a top-level key: `requestId`, `$.requestId` and
/// `requestId.contents` all name the top-level `requestId`.
fn named_field(rejection: &mut EnvelopeRejection, field: &str) {
    let path = field.trim().trim_start_matches("$.").trim_matches('"');
    let name = path.split('.').next().unwrap_or_default().trim();
    rejection.note(name);
}

/// Note fields named in the parser's own wording: `Unknown name "userAgent"` and
/// `Unknown name userAgent: Cannot find field.`
fn named_in_text(rejection: &mut EnvelopeRejection, text: &str) {
    const MARKER: &str = "Unknown name";
    let mut rest = text;
    while let Some(at) = rest.find(MARKER) {
        rest = rest[at + MARKER.len()..].trim_start();
        let name = match rest.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or_default(),
            None => rest
                .split(|c: char| c.is_ascii_whitespace() || c == ':' || c == ',' || c == '.')
                .next()
                .unwrap_or_default(),
        };
        rejection.note(name.trim());
    }
}

fn optional_key(name: &str) -> Option<&'static str> {
    DROPPABLE.iter().copied().find(|known| *known == name)
}

fn required_key(name: &str) -> Option<&'static str> {
    REQUIRED.iter().copied().find(|known| *known == name)
}

fn push_unique(fields: &mut Vec<&'static str>, field: &'static str) {
    if !fields.contains(&field) {
        fields.push(field);
    }
}

// ---------------------------------------------------------------------------
// Account refusals
// ---------------------------------------------------------------------------

/// Wording that means the provider retired the account.
///
/// Every phrase is long enough that ordinary failure text cannot produce it by accident:
/// matching on a bare `disabled` or `violation` would retire healthy accounts on a
/// coincidental word, and a false permanent verdict is far worse than a missed one (the
/// account keeps working under the status-quo path). Case-insensitive.
const PERMANENT_PHRASES: [&str; 7] = [
    "has been disabled in this account for violation",
    "service has been disabled in this account",
    "account has been deactivated",
    "your account has been suspended",
    "account permanently banned",
    "violation of our terms",
    "violation of terms",
];

/// Wording that means the account must pass provider verification. This is the one
/// signal that arrives on a perfectly healthy, fully-quota'd account, so it is matched
/// before any permanent phrase and can never be classified as one.
const VERIFICATION_PHRASES: [&str; 1] = ["verify your account to continue"];

/// Google's protobuf `status` enum value for a terms-of-service termination.
const PERMANENT_STATUS: &str = "TOS_VIOLATION";
/// Google's protobuf `status` enum value for an account that must be verified.
const VERIFICATION_STATUS: &str = "VALIDATION_REQUIRED";

/// Classify an upstream answer as a refusal of the *account* rather than of the request.
///
/// `http_status` is the status of the enclosing HTTP answer; `None` when the body arrived
/// inside a `200` (an SSE error frame or a wrapped buffered response), where the body's own
/// `error.code` is the only status available.
///
/// `None` is the status-quo path and the safe default: nothing is classified unless the
/// body names one of the taxonomy's signals. Two guards keep the classification honest:
/// a `5xx` or `429` is transient by definition and is never a refusal whatever its
/// wording, and the verification signal is read before the permanent ones so the healthy
/// case cannot fall through to "banned".
pub fn refusal(http_status: Option<u16>, body: &[u8]) -> Option<Refusal> {
    let value: Value = serde_json::from_slice(body).ok()?;
    refusal_in(http_status, &value)
}

/// The body's own error message, verbatim, for an operator-facing detail line.
///
/// Empty when the body is not JSON or carries no message: the refusal classification is
/// what the caller acts on, and the detail is only ever appositional.
pub fn refusal_detail(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .map(|value| error_message(&value).to_string())
        .unwrap_or_default()
}

/// [`refusal`] over an already-parsed error, for the response reader and the stream
/// decoder, which have the error object instead of the raw bytes.
fn refusal_in(http_status: Option<u16>, value: &Value) -> Option<Refusal> {
    let error = value.get("error").unwrap_or(value);
    let code = http_status.or_else(|| {
        error
            .get("code")
            .and_then(Value::as_u64)
            .and_then(|code| u16::try_from(code).ok())
    });
    // Transient by definition: a provider retry or an exhausted quota window can echo any
    // wording at all, so no phrase in such a body is evidence about the account.
    if matches!(code, Some(429 | 500..=599)) {
        return None;
    }
    let status = error
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status.eq_ignore_ascii_case(VERIFICATION_STATUS) {
        return Some(Refusal::Verification);
    }
    let message = error_message(value);
    let lower = message.to_ascii_lowercase();
    if VERIFICATION_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
    {
        return Some(Refusal::Verification);
    }
    if status.eq_ignore_ascii_case(PERMANENT_STATUS) {
        return Some(Refusal::Terminated);
    }
    if PERMANENT_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
    {
        return Some(Refusal::Terminated);
    }
    None
}

/// The `message` an error object carries, or the bare string body itself.
fn error_message(value: &Value) -> &str {
    let error = value.get("error").unwrap_or(value);
    match error {
        Value::String(text) => text,
        _ => error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    }
}

fn contents_in(messages: &[Message], system_parts: &mut Vec<Value>) -> Result<Vec<Value>> {
    let tool_names = tool_names(messages);
    let mut contents: Vec<Value> = Vec::with_capacity(messages.len());
    for message in messages {
        if message.role == Role::System {
            let text = message.joined_text();
            if !text.is_empty() {
                system_parts.push(json!({ "text": text }));
            }
            continue;
        }
        let role = match message.role {
            Role::Assistant => "model",
            Role::User | Role::Tool => "user",
            Role::System => unreachable!("handled above"),
        };
        let mut parts: Vec<Value> = Vec::with_capacity(message.parts.len());
        for part in &message.parts {
            parts.push(part_in(part, &tool_names)?);
        }
        parts.retain(|part| !part.is_null());
        if parts.is_empty() {
            continue;
        }
        if let Some(last) = contents.last_mut() {
            if last.get("role").and_then(Value::as_str) == Some(role) {
                if let Some(existing) = last.get_mut("parts").and_then(Value::as_array_mut) {
                    existing.extend(parts);
                    continue;
                }
            }
        }
        contents.push(json!({ "role": role, "parts": parts }));
    }
    Ok(contents)
}

fn tool_names(messages: &[Message]) -> std::collections::HashMap<&str, &str> {
    let mut names = std::collections::HashMap::new();
    for message in messages {
        for part in &message.parts {
            if let Part::ToolUse { id, name, .. } = part {
                names.insert(id.as_str(), name.as_str());
            }
        }
    }
    names
}

fn part_in(part: &Part, tool_names: &std::collections::HashMap<&str, &str>) -> Result<Value> {
    Ok(match part {
        Part::Text(text) => {
            if text.is_empty() {
                Value::Null
            } else {
                json!({ "text": text })
            }
        }
        Part::Image { url, media_type } => image_part(url, media_type),
        Part::ToolUse { name, input, .. } => json!({
            "functionCall": {"name": name, "args": if input.is_null() { json!({}) } else { input.clone() }},
        }),
        Part::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            let name = tool_names
                .get(tool_use_id.as_str())
                .copied()
                .unwrap_or(tool_use_id.as_str());
            let response = if *is_error {
                json!({"error": content})
            } else {
                json!({"result": content})
            };
            json!({
                "functionResponse": {"name": name, "response": response},
            })
        }
        Part::Reasoning { text, signature } => {
            if text.is_empty() {
                Value::Null
            } else {
                let mut block = Map::new();
                block.insert("text".into(), Value::from(text.clone()));
                block.insert("thought".into(), Value::Bool(true));
                if let Some(signature) = signature {
                    block.insert("thoughtSignature".into(), Value::from(signature.clone()));
                }
                Value::Object(block)
            }
        }
        Part::Opaque(value) => value.clone(),
    })
}

fn image_part(url: &str, media_type: &Option<String>) -> Value {
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((meta, data)) = rest.split_once(',') {
            let mime = meta
                .split(';')
                .next()
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .or_else(|| media_type.clone())
                .unwrap_or_else(|| "image/png".to_string());
            return json!({"inlineData": {"mimeType": mime, "data": data}});
        }
    }
    let mut file = Map::new();
    file.insert("fileUri".into(), Value::from(url.to_string()));
    if let Some(mime) = media_type.as_deref().filter(|value| !value.is_empty()) {
        file.insert("mimeType".into(), Value::from(mime.to_string()));
    }
    json!({ "fileData": Value::Object(file) })
}

// ---------------------------------------------------------------------------
// Response in
// ---------------------------------------------------------------------------

/// The Gemini payload: the `response` member when the CCA wrapper is present, else the
/// body itself.
fn payload(body: &Value) -> &Value {
    body.get("response")
        .filter(|value| value.is_object())
        .unwrap_or(body)
}

fn response_id(body: &Value, payload: &Value) -> String {
    if let Some(id) = payload.get("responseId").and_then(Value::as_str) {
        return id.to_string();
    }
    body.get("traceId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn response_model(payload: &Value) -> String {
    payload
        .get("modelVersion")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn candidate(body: &Value) -> Option<&Value> {
    payload(body)
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
}

/// Parse one buffered CCA response into the canonical form.
pub fn parse_response(body: &Value) -> Result<ChatResponse> {
    let payload = payload(body);
    if let Some(error) = payload.get("error") {
        return Err(error_from_value(error));
    }
    let candidate = candidate(body);
    let mut tool_index = 0;
    let parts = candidate
        .map(|candidate| parts_from_candidate(candidate, &mut tool_index))
        .unwrap_or_default();
    let saw_tool = parts
        .iter()
        .any(|part| matches!(part, Part::ToolUse { .. }));
    let finish_reason = match candidate
        .and_then(|c| c.get("finishReason"))
        .and_then(Value::as_str)
    {
        Some(reason) => finish_from_gemini(reason, saw_tool),
        None if saw_tool => FinishReason::ToolCalls,
        None => {
            if payload.get("promptFeedback").is_some() {
                FinishReason::ContentFilter
            } else {
                FinishReason::Stop
            }
        }
    };
    Ok(ChatResponse {
        id: response_id(body, payload),
        model: response_model(payload),
        parts,
        finish_reason,
        usage: usage_from(payload.get("usageMetadata")),
    })
}

fn parts_from_candidate(candidate: &Value, tool_index: &mut u32) -> Vec<Part> {
    let Some(blocks) = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut parts = Vec::with_capacity(blocks.len());
    for block in blocks {
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            if text.is_empty() {
                continue;
            }
            if block.get("thought").and_then(Value::as_bool) == Some(true) {
                parts.push(Part::Reasoning {
                    text: text.to_string(),
                    signature: block
                        .get("thoughtSignature")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                });
            } else {
                parts.push(Part::Text(text.to_string()));
            }
            continue;
        }
        if let Some(call) = block.get("functionCall") {
            let index = *tool_index;
            *tool_index += 1;
            parts.push(Part::ToolUse {
                id: format!("call_{index}"),
                name: call
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input: call.get("args").cloned().unwrap_or_else(|| json!({})),
            });
            continue;
        }
        parts.push(Part::Opaque(block.clone()));
    }
    parts
}

fn finish_from_gemini(reason: &str, saw_tool: bool) -> FinishReason {
    match reason {
        "STOP" if saw_tool => FinishReason::ToolCalls,
        "STOP" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
            FinishReason::ContentFilter
        }
        _ => FinishReason::Other,
    }
}

fn usage_from(usage: Option<&Value>) -> Usage {
    let Some(usage) = usage else {
        return Usage::default();
    };
    let count = |key: &str| {
        usage
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0)
    };
    Usage {
        input_tokens: count("promptTokenCount"),
        output_tokens: count("candidatesTokenCount").saturating_add(count("thoughtsTokenCount")),
        cache_read_tokens: count("cachedContentTokenCount"),
        cache_write_tokens: 0,
        reasoning_tokens: count("thoughtsTokenCount"),
    }
}

/// Provider label used in the translated error text.
const PROVIDER: &str = "google code assist";

fn error_from_value(error: &Value) -> Error {
    let detail = match error_message(error) {
        "" => "upstream error",
        text => text,
    };
    // A refusal of the account is not a translation failure: it is the one error class the
    // gateway must act on (retire the account, or cool it down) rather than pass through.
    if let Some(refusal) = refusal_in(None, error) {
        return Error::account_refused(PROVIDER, refusal, detail);
    }
    Error::Translation(format!("{PROVIDER}: {detail}"))
}

// ---------------------------------------------------------------------------
// Streaming in
// ---------------------------------------------------------------------------

/// Incremental decoder for `streamGenerateContent?alt=sse`.
#[derive(Debug, Default)]
pub struct GoogleCcaDecoder {
    codec: SseCodec,
    model: String,
    id: String,
    sent_start: bool,
    done: bool,
    usage: Option<Usage>,
    finish: Option<FinishReason>,
    tool_index: u32,
    saw_tool: bool,
}

impl GoogleCcaDecoder {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            ..Self::default()
        }
    }

    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<StreamEvent>) -> Result<()> {
        self.codec.push(chunk);
        while let Some(frame) = self.codec.next_frame() {
            self.frame(&frame, out)?;
        }
        Ok(())
    }

    pub fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<()> {
        while let Some(frame) = self.codec.finish() {
            self.frame(&frame, out)?;
        }
        self.emit_done(out);
        Ok(())
    }

    fn frame(&mut self, frame: &SseFrame, out: &mut Vec<StreamEvent>) -> Result<()> {
        if self.done {
            return Ok(());
        }
        if frame.is_done() {
            self.emit_done(out);
            return Ok(());
        }
        let Ok(value) = serde_json::from_str::<Value>(&frame.data) else {
            return Ok(()); // keep-alive or non-JSON payload
        };
        if let Some(error) = value
            .get("error")
            .or_else(|| value.get("response").and_then(|r| r.get("error")))
        {
            // A refusal raised from inside the body is the account's lifecycle verdict
            // arriving after the `200` that opened this stream. Nothing above the decoder
            // has seen it and nothing can fail over — the client already has the response —
            // but the gateway must still act on it exactly as it does on a `403` answer:
            // `Wire::close_with_error` keeps the class and `ResponseStream::commit` benches
            // the account and retires the row. Returning `Error::account_refused` here is
            // what carries that verdict across that boundary; a plain `Translation` error
            // would end the stream and leave a suspended account in rotation.
            return Err(error_from_value(error));
        }
        self.chunk(&value, out);
        Ok(())
    }

    fn chunk(&mut self, body: &Value, out: &mut Vec<StreamEvent>) {
        let payload = payload(body);
        if !self.sent_start {
            self.sent_start = true;
            self.id = response_id(body, payload);
            let model = response_model(payload);
            if !model.is_empty() {
                self.model = model;
            }
            out.push(StreamEvent::Start {
                id: self.id.clone(),
                model: self.model.clone(),
            });
        }
        if let Some(usage) = payload.get("usageMetadata") {
            self.usage = Some(usage_from(Some(usage)));
        }
        if let Some(candidate) = payload
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
        {
            self.candidate(candidate, out);
        }
    }

    fn candidate(&mut self, candidate: &Value, out: &mut Vec<StreamEvent>) {
        if let Some(blocks) = candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
        {
            for block in blocks {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    if text.is_empty() {
                        continue;
                    }
                    if block.get("thought").and_then(Value::as_bool) == Some(true) {
                        out.push(StreamEvent::ReasoningDelta(text.to_string()));
                    } else {
                        out.push(StreamEvent::TextDelta(text.to_string()));
                    }
                    continue;
                }
                if let Some(call) = block.get("functionCall") {
                    let index = self.tool_index;
                    self.tool_index += 1;
                    let name = call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
                    out.push(StreamEvent::ToolCallStart {
                        index,
                        id: format!("call_{index}"),
                        name,
                    });
                    let partial = serde_json::to_string(&args).unwrap_or_else(|_| "{}".into());
                    out.push(StreamEvent::ToolCallArgsDelta {
                        index,
                        partial_json: partial,
                    });
                    self.saw_tool = true;
                }
            }
        }
        if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
            self.finish = Some(finish_from_gemini(reason, self.saw_tool));
        }
    }

    fn emit_done(&mut self, out: &mut Vec<StreamEvent>) {
        if self.done {
            return;
        }
        if !self.sent_start {
            self.sent_start = true;
            out.push(StreamEvent::Start {
                id: self.id.clone(),
                model: self.model.clone(),
            });
        }
        self.done = true;
        let reason = self.finish.unwrap_or(if self.saw_tool {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        });
        out.push(StreamEvent::Done {
            reason,
            usage: self.usage,
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rrouter_core::{Dialect, Message, Part, ToolDef};

    fn request() -> ChatRequest {
        let mut req = ChatRequest::new("gemini-3-pro");
        req.system = Some("Be brief.".into());
        req.messages = vec![
            Message::text(Role::User, "weather in Paris?"),
            Message {
                role: Role::Assistant,
                parts: vec![Part::ToolUse {
                    id: "call_0".into(),
                    name: "get_weather".into(),
                    input: json!({"city": "Paris"}),
                }],
            },
            Message {
                role: Role::Tool,
                parts: vec![Part::ToolResult {
                    tool_use_id: "call_0".into(),
                    content: "18C".into(),
                    is_error: false,
                }],
            },
        ];
        req.tools = vec![ToolDef {
            name: "get_weather".into(),
            description: Some("Look up weather".into()),
            parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            strict: false,
        }];
        req.tool_choice = Some(ToolChoice::Tool("get_weather".into()));
        req.max_tokens = Some(1024);
        req.temperature = Some(0.2);
        req
    }

    /// The wire marker is the machine this build runs on, not one fixed host.
    ///
    /// The format is pinned rather than the values: a refactor may change which host is
    /// reported, never the marker shape, and never back to a hardcoded `darwin`.
    #[test]
    fn the_wire_marker_carries_the_resolved_host_tokens() {
        let identity = client_identity();
        let marker = user_agent();
        assert_eq!(
            marker,
            format!(
                "antigravity/hub/2.26.0 (aidev_client; os_type={}; arch={}; cl=1005016104)",
                identity.os_type(),
                identity.arch()
            ),
            "the marker is the resolved host, not a fixed arch"
        );
        assert!(
            marker.starts_with("antigravity/hub/2.26.0 (aidev_client; "),
            "{marker}"
        );
        assert!(marker.ends_with("; cl=1005016104)"), "{marker}");
        assert_eq!(marker.matches("os_type=").count(), 1, "{marker}");
        assert_eq!(marker.matches("arch=").count(), 1, "{marker}");
        assert!(
            !marker.contains("2.24.0"),
            "the product version does not follow the host: {marker}"
        );
        if std::env::consts::OS != "macos" {
            assert!(
                !marker.contains("darwin"),
                "a non-macOS host must not announce darwin: {marker}"
            );
        }
        assert_eq!(user_agent(), marker, "the cached string is stable");
    }

    /// Every host pair the enum can name resolves to its own platform code and marker,
    /// so the two layers of the client always describe the same machine.
    #[test]
    fn every_supported_host_has_one_identity() {
        let rows = [
            (("macos", "x86_64"), 1, "darwin", "x86_64"),
            (("macos", "aarch64"), 2, "darwin", "arm64"),
            (("linux", "x86_64"), 3, "linux", "x86_64"),
            (("linux", "aarch64"), 4, "linux", "aarch64"),
            (("windows", "x86_64"), 5, "windows", "x86_64"),
            (("windows", "aarch64"), 5, "windows", "x86_64"),
        ];
        for ((os, arch), platform, os_type, arch_token) in rows {
            let identity = identity_for(os, arch);
            assert_eq!(identity.platform(), platform, "{os}/{arch}");
            assert_eq!(identity.os_type(), os_type, "{os}/{arch}");
            assert_eq!(identity.arch(), arch_token, "{os}/{arch}");
            assert!(
                identity
                    .user_agent()
                    .contains(&format!("os_type={os_type}; arch={arch_token}; cl=")),
                "{}",
                identity.user_agent()
            );
        }
        assert_eq!(
            identity_for(std::env::consts::OS, std::env::consts::ARCH),
            client_identity(),
            "the table is the only source the resolved identity reads"
        );
    }

    /// A host the enum cannot name gets the documented fallback on both layers, never the
    /// `0` that used to stand for "no idea" and is not an enum member at all.
    #[test]
    fn an_unnamed_host_declares_the_documented_fallback() {
        let freebsd = identity_for("freebsd", "x86_64");
        assert_eq!(
            freebsd,
            identity_for("redox", "aarch64"),
            "one fallback identity, whatever the unrecognized host"
        );
        assert_eq!(
            (freebsd.platform(), freebsd.os_type(), freebsd.arch()),
            (3, "linux", "x86_64"),
            "the fallback is a real ClientMetadata.Platform member"
        );
        assert!(freebsd.user_agent().contains("os_type=linux; arch=x86_64;"));
    }

    #[test]
    fn builds_the_antigravity_envelope() {
        let envelope =
            build_request_with(&request(), "gemini-3-pro", "proj-1", "agent-test").unwrap();
        assert_eq!(envelope["model"], "gemini-3-pro");
        assert_eq!(envelope["userAgent"], ENVELOPE_USER_AGENT);
        assert_eq!(envelope["requestType"], "agent");
        assert_eq!(envelope["project"], "proj-1");
        assert_eq!(envelope["requestId"], "agent-test");
        let inner = &envelope["request"];
        assert!(inner.get("model").is_none(), "model is top-level only");
        assert!(
            inner.get("safetySettings").is_none(),
            "CCA must not send safetySettings"
        );
        assert_eq!(inner["contents"][0]["role"], "user");
        assert_eq!(
            inner["contents"][0]["parts"][0]["text"],
            "weather in Paris?"
        );
        assert_eq!(inner["contents"][1]["role"], "model");
        assert_eq!(
            inner["contents"][1]["parts"][0]["functionCall"]["name"],
            "get_weather"
        );
        assert_eq!(
            inner["contents"][2]["parts"][0]["functionResponse"]["name"], "get_weather",
            "a tool result is named from the assistant's call"
        );
        assert_eq!(inner["systemInstruction"]["parts"][0]["text"], "Be brief.");
        assert_eq!(inner["generationConfig"]["maxOutputTokens"], 1024);
        assert_eq!(inner["generationConfig"]["temperature"], 0.2);
        assert_eq!(
            inner["tools"][0]["functionDeclarations"][0]["name"],
            "get_weather"
        );
        assert_eq!(
            inner["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
            "get_weather"
        );
        assert!(
            envelope.get("sessionId").is_none(),
            "cloudcode-pa answers `Unknown name sessionId`: {envelope}"
        );
    }

    /// Regression: cloudcode-pa rejects `sessionId` with `400 INVALID_ARGUMENT`, so the
    /// serialized body — not just the in-memory map — must carry no such member, top-level
    /// or otherwise. The streaming path serializes the same builder, so one body covers it.
    #[test]
    fn the_serialized_envelope_has_no_session_id() {
        let envelope =
            build_request_with(&request(), "gemini-3-pro", "proj-1", "agent-test").unwrap();
        let serialized = envelope.to_string();
        assert!(!serialized.contains("sessionId"), "{serialized}");
        let body: Value = serde_json::from_str(&serialized).expect("round trip");
        let top_level: Vec<&str> = body
            .as_object()
            .expect("object envelope")
            .keys()
            .map(String::as_str)
            .filter(|key| *key != "sessionId")
            .collect();
        let mut sorted = top_level.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            ["model", "project", "request", "requestId", "requestType", "userAgent"],
            "the envelope is exactly what the current wire declares"
        );
        assert!(build_request(&request(), "gemini-3-pro", "proj-1").is_ok());
    }

    #[test]
    fn an_empty_project_is_a_client_error() {
        let err = build_request_with(&request(), "m", "  ", "agent-test").unwrap_err();
        assert!(matches!(err, Error::BadRequest(_)), "{err}");
    }

    #[test]
    fn parses_a_wrapped_response_with_usage_and_tools() {
        let body: Value =
            serde_json::from_str(include_str!("../tests/fixtures/google_cca_response.json"))
                .expect("fixture");
        let response = parse_response(&body).unwrap();
        assert_eq!(response.id, "resp-fixture-1");
        assert_eq!(response.model, "gemini-3-pro");
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(response.usage.output_tokens, 25);
        assert_eq!(response.usage.cache_read_tokens, 3);
        assert!(matches!(&response.parts[0], Part::Reasoning { text, .. } if text == "think"));
        assert!(matches!(&response.parts[1], Part::Text(text) if text == "18C"));
        assert!(matches!(
            &response.parts[2],
            Part::ToolUse { name, input, .. } if name == "get_weather" && input["city"] == "Paris"
        ));
    }

    #[test]
    fn accepts_a_bare_gemini_body() {
        let body = json!({
            "candidates": [{"content": {"parts": [{"text": "hi"}]}, "finishReason": "MAX_TOKENS"}]
        });
        let response = parse_response(&body).unwrap();
        assert_eq!(response.finish_reason, FinishReason::Length);
        assert!(matches!(&response.parts[0], Part::Text(text) if text == "hi"));
    }

    #[test]
    fn decodes_sse_with_fragmented_frames() {
        let wire = include_str!("../tests/fixtures/google_cca_stream.sse");

        let decode = |chunk: usize| {
            let mut decoder = GoogleCcaDecoder::new("gemini-3-pro");
            let mut events = Vec::new();
            for piece in wire.as_bytes().chunks(chunk) {
                decoder.push(piece, &mut events).unwrap();
            }
            decoder.finish(&mut events).unwrap();
            events
        };
        let events = decode(5);
        assert_eq!(
            events,
            vec![
                StreamEvent::Start {
                    id: "r".into(),
                    model: "gemini-3-pro".into()
                },
                StreamEvent::TextDelta("Hel".into()),
                StreamEvent::TextDelta("lo".into()),
                StreamEvent::Done {
                    reason: FinishReason::Stop,
                    usage: Some(Usage {
                        input_tokens: 7,
                        output_tokens: 2,
                        ..Usage::default()
                    })
                },
            ]
        );
        assert_eq!(
            events,
            decode(wire.len()),
            "chunk boundaries cannot change events"
        );
    }

    #[test]
    fn decodes_streamed_function_calls() {
        let frame = format!(
            "data: {}\n\n",
            json!({"response": {"candidates": [{
                "content": {"parts": [{"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}]},
                "finishReason": "STOP"
            }]}})
        );
        let mut decoder = GoogleCcaDecoder::new("m");
        let mut events = Vec::new();
        decoder.push(frame.as_bytes(), &mut events).unwrap();
        decoder.finish(&mut events).unwrap();
        assert!(matches!(
            &events[1],
            StreamEvent::ToolCallStart { index: 0, name, .. } if name == "get_weather"
        ));
        assert!(matches!(
            &events[2],
            StreamEvent::ToolCallArgsDelta { index: 0, partial_json }
                if partial_json.contains("Paris")
        ));
        assert_eq!(
            events.last(),
            Some(&StreamEvent::Done {
                reason: FinishReason::ToolCalls,
                usage: None
            })
        );
    }

    #[test]
    fn openai_client_sees_text_tools_and_usage() {
        let inbound = json!({
            "model": "gemini-3-pro",
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [{"type": "function", "function": {
                "name": "get_weather",
                "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
            }}],
            "stream": false
        });
        let request = crate::parse_request(Dialect::OpenAiChat, &inbound).unwrap();
        let envelope = build_request_with(&request, "gemini-3-pro", "proj", "agent-1").unwrap();
        assert_eq!(
            envelope["request"]["tools"][0]["functionDeclarations"][0]["name"],
            "get_weather"
        );

        let upstream = json!({"response": {"candidates": [{
            "content": {"parts": [
                {"text": "18C"},
                {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}
            ]},
            "finishReason": "STOP"
        }], "usageMetadata": {"promptTokenCount": 4, "candidatesTokenCount": 5}}});
        let response = crate::parse_response(Dialect::GoogleCodeAssist, &upstream).unwrap();
        let out = crate::build_response(Dialect::OpenAiChat, &response).unwrap();
        assert_eq!(out["choices"][0]["message"]["content"], "18C");
        assert_eq!(
            out["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        assert_eq!(
            out["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"city\":\"Paris\"}"
        );
        assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(out["usage"]["prompt_tokens"], 4);
        assert_eq!(out["usage"]["completion_tokens"], 5);
    }

    #[test]
    fn anthropic_client_sees_reasoning_tools_and_usage() {
        let inbound = json!({
            "model": "gemini-3-pro",
            "max_tokens": 256,
            "system": "be terse",
            "messages": [{"role": "user", "content": "weather?"}]
        });
        let request = crate::parse_request(Dialect::AnthropicMessages, &inbound).unwrap();
        let envelope = build_request_with(&request, "gemini-3-pro", "proj", "agent-2").unwrap();
        assert_eq!(
            envelope["request"]["systemInstruction"]["parts"][0]["text"],
            "be terse"
        );
        assert_eq!(
            envelope["request"]["generationConfig"]["maxOutputTokens"],
            256
        );

        let upstream = json!({"response": {"candidates": [{
            "content": {"parts": [
                {"text": "thinking", "thought": true},
                {"text": "18C"},
                {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}
            ]},
            "finishReason": "STOP"
        }], "usageMetadata": {"promptTokenCount": 9, "candidatesTokenCount": 3, "thoughtsTokenCount": 2}}});
        let response = crate::parse_response(Dialect::GoogleCodeAssist, &upstream).unwrap();
        let out = crate::build_response(Dialect::AnthropicMessages, &response).unwrap();
        assert_eq!(out["content"][0]["type"], "thinking");
        assert_eq!(out["content"][0]["thinking"], "thinking");
        assert_eq!(out["content"][1]["type"], "text");
        assert_eq!(out["content"][1]["text"], "18C");
        assert_eq!(out["content"][2]["type"], "tool_use");
        assert_eq!(out["content"][2]["name"], "get_weather");
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(out["usage"]["input_tokens"], 9);
        assert_eq!(out["usage"]["output_tokens"], 5);
    }

    #[test]
    fn streaming_decodes_into_the_clients_own_frames() {
        let wire = "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hi\"}]}}]}}\n\n\
                    data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"!\"}]},\"finishReason\":\"STOP\"}]}}\n\n";
        let mut decoder = crate::StreamDecoder::new(Dialect::GoogleCodeAssist, "m");
        let mut events = decoder.push(wire.as_bytes()).unwrap();
        events.extend(decoder.finish().unwrap());

        let mut openai = crate::StreamEncoder::new(Dialect::OpenAiChat, "m");
        let mut openai_wire = String::new();
        for event in &events {
            openai_wire.push_str(&String::from_utf8_lossy(&openai.encode(event).unwrap()));
        }
        assert!(openai_wire.contains("Hi"), "{openai_wire}");
        assert!(openai_wire.contains("!"), "both deltas reach the client");
        assert!(openai_wire.contains("[DONE]"), "{openai_wire}");

        let mut anthropic = crate::StreamEncoder::new(Dialect::AnthropicMessages, "m");
        let mut anthropic_wire = String::new();
        for event in &events {
            anthropic_wire.push_str(&String::from_utf8_lossy(&anthropic.encode(event).unwrap()));
        }
        assert!(
            anthropic_wire.contains("content_block_delta"),
            "{anthropic_wire}"
        );
        assert!(anthropic_wire.contains("message_stop"), "{anthropic_wire}");
    }

    #[test]
    fn the_field_this_build_used_to_send_is_no_longer_repairable() {
        // The exact answer cloudcode-pa gives for `sessionId`. The envelope no longer sends
        // it, so it is not a member repair may act on: naming it must not put it back on
        // any later request build, and it must not fail the account either.
        let body = json!({"error": {
            "code": 400,
            "message": "Invalid JSON payload received. Unknown name \"sessionId\": Cannot find field.",
            "status": "INVALID_ARGUMENT",
            "details": [{
                "@type": "type.googleapis.com/google.rpc.BadRequest",
                "fieldViolations": [{
                    "field": "sessionId",
                    "description": "Invalid JSON payload received. Unknown name \"sessionId\"."
                }]
            }]
        }});
        let rejection = EnvelopeRejection::parse(body.to_string().as_bytes());
        assert!(
            rejection.is_empty(),
            "sessionId is neither optional nor identity any more: {rejection:?}"
        );
        assert!(rejected_fields(body.to_string().as_bytes()).is_empty());
        assert!(rejection.error().is_none(), "an absent member is no reason to fail");
    }

    #[test]
    fn the_violation_path_or_the_message_alone_is_enough() {
        let by_violation = json!({"error": {
            "status": "INVALID_ARGUMENT",
            "details": [{
                "@type": "type.googleapis.com/google.rpc.BadRequest",
                "fieldViolations": [{"field": "$.requestId.foo"}, {"field": "sessionId"}]
            }]
        }});
        let rejection = EnvelopeRejection::parse(by_violation.to_string().as_bytes());
        assert_eq!(
            rejection.required(),
            ["requestId"],
            "a path names its first segment, and requestId is identity"
        );
        assert!(
            rejection.droppable().is_empty(),
            "the envelope has no optional member left to drop"
        );
        assert!(rejection.fields_to_drop().is_empty());

        let by_message = json!({"error": {
            "code": 400,
            "status": "INVALID_ARGUMENT",
            "message": "Invalid JSON payload received. Unknown name userAgent: Cannot find field."
        }});
        assert!(
            rejected_fields(by_message.to_string().as_bytes()).is_empty(),
            "the parser's own wording is read, and identity is never droppable"
        );
        assert_eq!(
            EnvelopeRejection::parse(by_message.to_string().as_bytes()).required(),
            ["userAgent"]
        );
    }

    #[test]
    fn only_droppable_top_level_fields_survive() {
        let inner = json!({"error": {
            "status": "INVALID_ARGUMENT",
            "message": "Invalid JSON payload received. Unknown name \"contents\": Cannot find field.",
            "details": [{
                "@type": "type.googleapis.com/google.rpc.BadRequest",
                "fieldViolations": [
                    {"field": "request.contents"},
                    {"field": "toolConfig"},
                    {"field": "userAgent"}
                ]
            }]
        }});
        let rejection = EnvelopeRejection::parse(inner.to_string().as_bytes());
        assert!(
            rejection.droppable().is_empty(),
            "nothing under request and nothing outside the allowlist is ever dropped"
        );
        assert_eq!(
            rejection.required(),
            ["userAgent"],
            "an identity field is reported, never repaired"
        );

        let not_invalid = json!({"error": {
            "code": 400,
            "message": "Invalid JSON payload received. Unknown name \"sessionId\".",
            "status": "FAILED_PRECONDITION"
        }});
        assert!(rejected_fields(not_invalid.to_string().as_bytes()).is_empty());
        assert!(rejected_fields(b"not json").is_empty());
        assert!(rejected_fields(b"{}").is_empty());
    }

    #[test]
    fn an_identity_violation_is_a_typed_error_not_a_drop() {
        for identity in ["userAgent", "requestId", "model", "requestType", "project"] {
            let body = json!({"error": {
                "code": 400,
                "status": "INVALID_ARGUMENT",
                "message": format!("Invalid JSON payload received. Unknown name \"{identity}\": Cannot find field."),
                "details": [{
                    "@type": "type.googleapis.com/google.rpc.BadRequest",
                    "fieldViolations": [{"field": identity}]
                }]
            }});
            let rejection = EnvelopeRejection::parse(body.to_string().as_bytes());
            assert!(
                rejected_fields(body.to_string().as_bytes()).is_empty(),
                "repair must not be offered a field the envelope cannot lose: {identity}"
            );
            assert!(rejection.droppable().is_empty(), "{identity}");
            assert_eq!(rejection.required(), [identity]);
            assert!(!rejection.is_empty());

            let error = rejection.error().expect("an identity refusal is fatal");
            assert_eq!(error.fields(), [identity]);
            let mapped = error.clone().into_error();
            assert_eq!(
                mapped.status(),
                400,
                "the upstream answer is passed through"
            );
            assert!(mapped.to_string().contains(identity), "{mapped}");
        }
    }

    #[test]
    fn a_violation_under_request_is_ignored() {
        let body = json!({"error": {
            "code": 400,
            "status": "INVALID_ARGUMENT",
            "message": "Invalid JSON payload received. Unknown name \"request.contents\": Cannot find field.",
            "details": [{
                "@type": "type.googleapis.com/google.rpc.BadRequest",
                "fieldViolations": [
                    {"field": "request.contents"},
                    {"field": "request.generationConfig.topP"}
                ]
            }]
        }});
        let rejection = EnvelopeRejection::parse(body.to_string().as_bytes());
        assert!(rejection.is_empty(), "{rejection:?}");
        assert!(rejection.fields_to_drop().is_empty());
        assert!(rejection.error().is_none());
        assert!(rejected_fields(body.to_string().as_bytes()).is_empty());
    }

    #[test]
    fn mixed_violations_drop_nothing_and_report_the_identity_field() {
        let body = json!({"error": {
            "code": 400,
            "status": "INVALID_ARGUMENT",
            "details": [{
                "@type": "type.googleapis.com/google.rpc.BadRequest",
                "fieldViolations": [{"field": "sessionId"}, {"field": "userAgent"}]
            }]
        }});
        let rejection = EnvelopeRejection::parse(body.to_string().as_bytes());
        assert!(rejection.droppable().is_empty());
        assert_eq!(rejection.required(), ["userAgent"]);
        assert!(
            rejection.fields_to_drop().is_empty(),
            "a broken identity makes the whole answer fatal"
        );
        assert_eq!(
            rejection.error().map(|error| error.fields().to_vec()),
            Some(vec!["userAgent"])
        );

        let envelope = build_request_with(&request(), "gemini-3-pro", "proj-1", "agent-test")
            .expect("envelope");
        assert_eq!(
            without_fields(envelope.clone(), rejection.fields_to_drop()),
            envelope,
            "the resend is the envelope untouched"
        );
    }

    #[test]
    fn without_fields_refuses_identity_even_when_asked() {
        let envelope = build_request_with(&request(), "gemini-3-pro", "proj-1", "agent-test")
            .expect("envelope");
        let repaired = without_fields(
            envelope.clone(),
            &[
                "model",
                "userAgent",
                "requestType",
                "project",
                "requestId",
                "request",
                "sessionId",
            ],
        );
        assert!(
            repaired.get("sessionId").is_none(),
            "a name outside DROPPABLE is never removed, and never added either"
        );
        for identity in REQUIRED {
            assert!(
                repaired.get(identity).is_some(),
                "repair cannot strip {identity}: {repaired}"
            );
        }
        assert!(repaired.get("request").is_some(), "{repaired}");
        assert_eq!(repaired["model"], envelope["model"]);
        assert_eq!(repaired["requestId"], envelope["requestId"]);
    }

    #[test]
    fn rebuilding_without_fields_keeps_everything_else() {
        let envelope = build_request_with(&request(), "gemini-3-pro", "proj-1", "agent-test")
            .expect("envelope");
        assert!(envelope.get("sessionId").is_none());

        let repaired = without_fields(envelope.clone(), &["sessionId"]);
        assert_eq!(
            repaired, envelope,
            "asking to drop an absent member changes nothing"
        );
        assert_eq!(repaired["model"], "gemini-3-pro");
        assert_eq!(repaired["requestId"], "agent-test");
        assert_eq!(
            repaired["request"]["contents"][0]["parts"][0]["text"],
            "weather in Paris?"
        );

        assert_eq!(
            without_fields(envelope.clone(), &[]),
            envelope,
            "no fields to drop is the envelope unchanged"
        );
    }

    #[test]
    fn stream_errors_are_reported_not_swallowed() {
        let frame = format!(
            "data: {}\n\n",
            json!({"error": {"code": 429, "message": "quota exhausted", "status": "RESOURCE_EXHAUSTED"}})
        );
        let mut decoder = GoogleCcaDecoder::new("m");
        let mut events = Vec::new();
        let err = decoder.push(frame.as_bytes(), &mut events).unwrap_err();
        assert!(err.to_string().contains("quota exhausted"), "{err}");
    }

    /// The three classes the taxonomy distinguishes, read from the same two carriers the
    /// rest of this module reads: the protobuf `status` enum and the message text.
    #[test]
    fn a_permanent_termination_is_classified_from_status_or_wording() {
        let by_status = json!({
            "error": {
                "code": 403,
                "status": "TOS_VIOLATION",
                "message": "The caller does not have permission"
            }
        });
        assert_eq!(
            refusal(Some(403), by_status.to_string().as_bytes()),
            Some(Refusal::Terminated),
            "the protobuf status alone is enough"
        );

        // Google's real wording, in the shape the account endpoint returns it.
        let by_message = json!({
            "error": {
                "code": 403,
                "message": "The service has been disabled in this account for violation of our \
                            Terms of Service."
            }
        });
        assert_eq!(
            refusal(Some(403), by_message.to_string().as_bytes()),
            Some(Refusal::Terminated)
        );
        // Each phrase is matched case-insensitively.
        let upper = json!({"error": {"message": "YOUR ACCOUNT HAS BEEN SUSPENDED."}});
        assert_eq!(
            refusal(Some(403), upper.to_string().as_bytes()),
            Some(Refusal::Terminated)
        );
        let deactivated = json!({"error": {"message": "This account has been deactivated."}});
        assert_eq!(
            refusal(Some(400), deactivated.to_string().as_bytes()),
            Some(Refusal::Terminated)
        );
    }

    /// Verification-required is recoverable even though it arrives on healthy accounts, and
    /// it must never be read as a termination — the one false positive that would retire a
    /// working account.
    #[test]
    fn a_verification_requirement_is_recoverable_and_never_permanent() {
        let by_status = json!({
            "error": {
                "code": 403,
                "status": "VALIDATION_REQUIRED",
                "message": "Something went wrong"
            }
        });
        assert_eq!(
            refusal(Some(403), by_status.to_string().as_bytes()),
            Some(Refusal::Verification)
        );

        let by_message = json!({
            "error": {"code": 403, "message": "Please verify your account to continue."}
        });
        assert_eq!(
            refusal(Some(403), by_message.to_string().as_bytes()),
            Some(Refusal::Verification)
        );
        // The verification signal wins even when the same body also carries phrasing the
        // permanent list would otherwise match.
        let both = json!({
            "error": {
                "status": "VALIDATION_REQUIRED",
                "message": "verify your account to continue: suspected violation of terms"
            }
        });
        assert_eq!(
            refusal(Some(403), both.to_string().as_bytes()),
            Some(Refusal::Verification),
            "verification is checked before any permanent signal"
        );
    }

    /// The status quo: everything else stays an ordinary upstream failure, including the
    /// transient answers whose bodies can echo arbitrary wording.
    #[test]
    fn ordinary_failures_are_not_refusals() {
        let cases = [
            (
                Some(429u16),
                json!({"error": {"code": 429, "message": "quota exhausted"}}),
            ),
            (
                Some(500),
                json!({"error": {"code": 500, "message": "internal error"}}),
            ),
            // A 5xx that happens to echo the wording is still transient, not a ban.
            (
                Some(503),
                json!({"error": {"message": "service has been disabled in this account"}}),
            ),
            (
                Some(400),
                json!({"error": {"status": "INVALID_ARGUMENT", "message": "Unknown name"}}),
            ),
            (
                Some(403),
                json!({"error": {"message": "The caller does not have permission"}}),
            ),
            (None, json!({"error": {"message": "quota exhausted"}})),
        ];
        for (status, body) in cases {
            assert_eq!(
                refusal(status, body.to_string().as_bytes()),
                None,
                "status {status:?} body {body}"
            );
        }
        assert_eq!(refusal(Some(403), b"not json"), None);
        assert_eq!(refusal(Some(403), b""), None);
        // A bare string body is read too, so a proxy-shaped error still classifies.
        assert_eq!(
            refusal(Some(403), br#""Your account has been suspended""#),
            Some(Refusal::Terminated)
        );
        assert_eq!(
            refusal_detail(
                json!({"error": {"message": "The service has been disabled"}})
                    .to_string()
                    .as_bytes()
            ),
            "The service has been disabled",
            "the detail keeps the provider's own wording"
        );
        assert_eq!(refusal_detail(b"not json"), "");
    }

    /// A refusal inside an SSE frame or a wrapped buffered body has no HTTP status of its
    /// own; the readers must still name it instead of flattening it to a translation error.
    #[test]
    fn a_streamed_refusal_becomes_an_account_refusal() {
        let payload = json!({"error": {"code": 403, "status": "TOS_VIOLATION", "message": "off"}});
        let frame = format!(
            r#"data: {payload}

"#
        );
        let mut decoder = GoogleCcaDecoder::new("m");
        let mut events = Vec::new();
        let err = decoder.push(frame.as_bytes(), &mut events).unwrap_err();
        assert_eq!(
            err.refusal(),
            Some(Refusal::Terminated),
            "the frame carries its own status: {err}"
        );

        let body = json!({
            "error": {"code": 403, "message": "Please verify your account to continue"}
        });
        let err = parse_response(&body).expect_err("a wrapped error is not a response");
        assert_eq!(err.refusal(), Some(Refusal::Verification));
        assert!(
            err.to_string().contains("needs verification"),
            "a wrapped verification error must read as verification, not as a translation failure"
        );
    }

    #[test]
    fn adapts_json_schema_type_unions_for_gemini_proto() {
        let mut req = ChatRequest::new("gemini-3-pro");
        req.messages = vec![Message::text(Role::User, "run grep")];
        req.tools = vec![ToolDef {
            name: "grep".into(),
            description: Some("Search files".into()),
            parameters: json!({
                "type": "object",
                "properties": {
                    "skip": {
                        "type": ["number", "null"]
                    },
                    "patterns": {
                        "type": "array",
                        "items": {
                            "type": ["string", "integer"]
                        }
                    },
                    "type": {
                        "type": "string",
                        "enum": ["a", "b"]
                    }
                }
            }),
            strict: false,
        }];

        let envelope =
            build_request_with(&req, "gemini-3-pro", "proj-1", "agent-test").expect("envelope");
        let parameters = &envelope["request"]["tools"][0]["functionDeclarations"][0]["parameters"];

        assert_eq!(
            parameters["properties"]["skip"],
            json!({
                "type": "number",
                "nullable": true
            })
        );
        assert_eq!(
            parameters["properties"]["patterns"]["items"],
            json!({
                "anyOf": [
                    {"type": "string"},
                    {"type": "integer"}
                ]
            })
        );
        assert_eq!(
            parameters["properties"]["type"],
            json!({
                "type": "string",
                "enum": ["a", "b"]
            })
        );
    }

    #[test]
    fn gemini_schema_handles_edge_cases_and_keywords() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "empty_type": { "type": [] },
                "null_only": { "type": ["null"] },
                "multi_with_existing_anyof": {
                    "type": ["string", "integer"],
                    "anyOf": [{"minLength": 2}]
                },
                "multi_with_null": {
                    "type": ["boolean", "number", "null"]
                },
                "nested_defs": {
                    "$defs": {
                        "leaf": { "type": ["integer", "null"] }
                    },
                    "definitions": {
                        "old_leaf": { "type": ["string", "null"] }
                    },
                    "patternProperties": {
                        "^x-": { "type": ["boolean", "null"] }
                    },
                    "additionalProperties": {
                        "type": ["string", "null"]
                    },
                    "not": {
                        "type": ["number", "null"]
                    },
                    "oneOf": [
                        { "type": ["string", "null"] }
                    ],
                    "allOf": [
                        { "type": ["integer", "null"] }
                    ],
                    "prefixItems": [
                        { "type": ["boolean", "null"] }
                    ],
                    "properties": {
                        "tuple_items": {
                            "type": "array",
                            "items": [
                                { "type": ["string", "null"] }
                            ]
                        }
                    }
                }
            }
        });

        gemini_schema(&mut schema);

        let p = &schema["properties"];
        assert_eq!(p["empty_type"], json!({"type": "null"}));
        assert_eq!(p["null_only"], json!({"type": "null", "nullable": true}));
        assert_eq!(
            p["multi_with_existing_anyof"],
            json!({
                "type": "string",
                "anyOf": [{"minLength": 2}]
            })
        );
        assert_eq!(
            p["multi_with_null"],
            json!({
                "anyOf": [{"type": "boolean"}, {"type": "number"}],
                "nullable": true
            })
        );
        let nested = &p["nested_defs"];
        assert_eq!(
            nested["$defs"]["leaf"],
            json!({"type": "integer", "nullable": true})
        );
        assert_eq!(
            nested["definitions"]["old_leaf"],
            json!({"type": "string", "nullable": true})
        );
        assert_eq!(
            nested["patternProperties"]["^x-"],
            json!({"type": "boolean", "nullable": true})
        );
        assert_eq!(
            nested["additionalProperties"],
            json!({"type": "string", "nullable": true})
        );
        assert_eq!(nested["not"], json!({"type": "number", "nullable": true}));
        assert_eq!(
            nested["oneOf"][0],
            json!({"type": "string", "nullable": true})
        );
        assert_eq!(
            nested["allOf"][0],
            json!({"type": "integer", "nullable": true})
        );
        assert_eq!(
            nested["prefixItems"][0],
            json!({"type": "boolean", "nullable": true})
        );
        assert_eq!(
            nested["properties"]["tuple_items"]["items"][0],
            json!({"type": "string", "nullable": true})
        );
    }
}
