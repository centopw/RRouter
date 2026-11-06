# RRouter — Foundation contract (T1, owner: coordinator)

State at dispatch: `cargo build` green, `cargo test -p rrouter-core` green (6 tests).

## Workspace
`/Users/cento/git/personal/RRouter` — Cargo workspace, 6 path crates, all version `0.1.0`,
edition 2021. Every dependency version is centralized in root `Cargo.toml`
`[workspace.dependencies]`; a crate adds deps with `foo.workspace = true`.

| crate | responsibility |
|---|---|
| `rrouter-core` | canonical model, dialect enum, error, SSE codec (**done**) |
| `rrouter-translate` | wire-dialect ↔ canonical model, streaming included |
| `rrouter-auth` | provider registry, credential material, mutex-locked token refresh |
| `rrouter-store` | SQLite persistence, AES-256-GCM credential encryption, API keys, usage |
| `rrouter-router` | routing (model → candidates), fallback, circuit breaker |
| `rrouter-gateway` | axum HTTP server + binary `rrouter` |

## `rrouter-core` API you may rely on (frozen — do not edit; message the coordinator)

```rust
pub enum Dialect { OpenAiChat, OpenAiResponses, AnthropicMessages, OpenAiEmbeddings }
impl Dialect { pub fn from_path(&str) -> Option<Self>; pub const fn as_str(self) -> &'static str;
               pub const fn is_event_framed(self) -> bool; }   // Display impl

pub enum Error { BadRequest(String), Unauthorized(String), NotFound(String),
                 NoRoute{model:String,detail:String},
                 Upstream{provider:String,status:u16,body:String,payload:Option<serde_json::Value>},
                 Translation(String), Store(String), Transport(String), Timeout(String),
                 Json(serde_json::Error), Io(std::io::Error) }
impl Error { pub fn status(&self)->u16; pub fn is_retryable_upstream(&self)->bool;
             pub fn is_rate_limited(&self)->bool; }
pub type Result<T, E = Error> = std::result::Result<T, E>;

pub enum Part { Text(String), Image{url:String,media_type:Option<String>},
                ToolUse{id:String,name:String,input:serde_json::Value},
                ToolResult{tool_use_id:String,content:String,is_error:bool},
                Reasoning{text:String,signature:Option<String>}, Opaque(serde_json::Value) }
pub enum Role { System, User, Assistant, Tool }
pub struct Message { pub role: Role, pub parts: Vec<Part> }  // Message::text(role, s), .joined_text()
pub struct ToolDef { pub name: String, pub description: Option<String>, pub parameters: serde_json::Value }
pub enum ToolChoice { Auto, None, Required, Tool(String) }
pub struct ChatRequest { pub model: String, pub system: Option<String>, pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>, pub tool_choice: Option<ToolChoice>, pub max_tokens: Option<u32>,
    pub temperature: Option<f64>, pub top_p: Option<f64>, pub stop: Vec<String>, pub stream: bool,
    pub extra: Option<(Dialect, serde_json::Value)> }   // ChatRequest::new(model)
pub enum StreamEvent { Start{id:String,model:String}, TextDelta(String), ReasoningDelta(String),
    ToolCallStart{index:u32,id:String,name:String}, ToolCallArgsDelta{index:u32,partial_json:String},
    Done{reason:FinishReason, usage:Option<Usage>} }
pub enum FinishReason { Stop, Length, ToolCalls, ContentFilter, Other } // .openai() .anthropic()
pub struct Usage { pub input_tokens:u32, pub output_tokens:u32,
                   pub cache_read_tokens:u32, pub cache_write_tokens:u32 }  // Fn::default(), .total()
pub struct ChatResponse { pub id:String, pub model:String, pub parts:Vec<Part>,
                          pub finish_reason:FinishReason, pub usage:Usage }
pub struct SseCodec { /* new(), push(&[u8]), next_frame()->Option<SseFrame>, finish(), buffered_len() */ }
pub struct SseFrame { pub event: Option<String>, pub data: String }  // .is_done() == data=="[DONE]"
pub fn render_openai(data:&str, event:Option<&str>) -> String;
pub fn render_anthropic(event:&str, data:&str) -> String;
```

## Hard rules for every worker
- Path-scoped: ONLY create/edit files inside your own crate directory. Never touch
  root `Cargo.toml`, `README.md`, or another crate. Need a new dependency or a core
  type change → message the coordinator instead of editing.
- Do NOT run `cargo build`/`test` for the whole workspace and never run
  `cargo clippy --fix` or any auto-formatter over other crates. Verify with
  `cargo test -p <your-crate>` and `cargo clippy -p <your-crate> -- -D warnings`.
- No `unwrap()`/`expect()` on runtime data paths (tests are fine). No `panic!` on
  client or upstream input. Return `rrouter_core::Error`.
- Boring, flat design. No trait objects where an enum works. No speculative traits
  with one implementation. No `Box<dyn>` for things decided at compile time.
- Performance is product: preallocate, avoid intermediate `String` clones of bodies,
  stream with `bytes::Bytes`, never buffer a whole response body to re-emit it.
- Report back: what compiled, exact command output summary, public API list,
  and anything the next crate must know. If something in the contract is
  insufficient, say precisely what is missing.
