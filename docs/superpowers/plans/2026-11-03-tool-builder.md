# Tool Builder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a tool builder that creates and edits every tool, shows the exact `tools/list` JSON live, and tests a draft through the real call code.

**Architecture:** The core crate gets the input-row model and the schema builder. The server gets
encrypted upstream auth, an address guard for upstream calls, one shared call function that also
returns an HTTP trace, and REST routes for tools and manual APIs. The web app gets a builder page
and an onboarding entry with a WAITING / CONNECTED badge.

**Tech Stack:** Rust (axum 0.8, rmcp 3.8, sqlx 0.9, reqwest 0.13, `aes-gcm`), React + Vite + TypeScript.

**Spec:** `docs/superpowers/specs/2026-11-02-tool-builder-design.md`

## Global Constraints

- All work is on the local `main` branch, one commit per task. Do not push.
- Before each commit: run the checks, scan the staged diff for secrets, and keep comments few, short, and in plain technical English.
- Checks: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (needs `docker compose up -d --wait postgres` and `DATABASE_URL`), `cd web && npm run lint && npm run build`.
- Errors: `thiserror` enums, lowercase messages without a final period. Handlers return `Result<_, ApiError>`. New `ApiError` variants need a status and a `snake_case` code in `status()` and `code()`.
- No `unwrap()` or `expect()` outside tests.
- New dependency: `aes-gcm` only (approved). `zeroize` needs approval first (see Task 1).
- Tests use `#[sqlx::test(migrations = "../../migrations")]`. The fake upstream is an axum router on `127.0.0.1:0`, as in `crates/server/tests/mcp.rs`.
- New API types derive `TS` with `#[ts(export)]`. `cargo test` writes them to `web/src/types/`. Commit the generated files.
- Never return a secret value from an API route, and never log it.
- Error text for a blocked address: `the upstream address is not allowed`.
- Onboarding badge copy: `WAITING`, `Waiting for your agent…`, `Paste this into your terminal:`, `CONNECTED`, `Connected · <client>`, `First request received. Now ask Claude something.`, `tools/list · <n> tools sent`.

## Review Focus

1. A DNS name that resolves to a private address, and a redirect from a public URL to `http://169.254.169.254/`: both must be refused. (Task 2)
2. Editing an imported tool with an enum or nested-object input: the saved schema must keep the enum and the nested object. (Task 3, Task 6)
3. A Test call with a secret header: the trace must show `••••`, and no API response may contain the secret. (Task 5, Task 6)
4. A re-import after the user renamed an edited tool: the tool must keep its new name and its edits. (Task 7)
5. Two browser tabs save the same tool: the second save must still produce a valid tool, and the open MCP sessions get `list_changed` each time. (Task 6)

---

### Task 1: Upstream secrets and the schema migration

**Files:**
- Create: `migrations/20261103000000_tool_builder.sql`
- Create: `crates/server/src/secrets.rs`
- Modify: `crates/server/Cargo.toml`, `crates/server/src/config.rs`, `crates/server/src/state.rs`, `crates/server/src/lib.rs`, `crates/server/src/error.rs` (`StartupError`), `crates/server/tests/common/mod.rs`, `.env.example`, `compose.yaml`
- Test: `crates/server/src/secrets.rs` (unit), `crates/server/tests/migrations.rs`

**Interfaces:**
- Produces:
  - `pub struct SecretsKey` (no `Debug`, no `Clone` of the raw bytes outside the crate). `SecretsKey::from_hex(&str) -> Result<Self, SecretsError>`.
  - `pub fn seal(key: &SecretsKey, api_id: i64, plain: &str) -> Result<Vec<u8>, SecretsError>`
  - `pub fn open(key: &SecretsKey, api_id: i64, sealed: &[u8]) -> Result<Secret, SecretsError>`
  - `pub struct Secret` with `pub fn expose(&self) -> &str`. No `Debug`, no `Display`, no `Serialize`.
  - `SecretsError`: `BadKey`, `UnknownVersion(u8)`, `Malformed`, `Decrypt`.
  - `Config.secrets_key: String` (env `SECRETS_KEY`, required). `AppState.secrets: Arc<SecretsKey>`.
  - Tables after the migration (exact SQL in Step 3).

- [ ] **Step 1: Ask the user about `zeroize`.** The design wants `Secret` to wipe its memory on drop. Ask: "May I add `zeroize` as a direct dependency? It is already in `Cargo.lock` through other crates." If yes, `Secret` wraps `zeroize::Zeroizing<String>`. If no, `Secret` wraps a plain `String`, and the commit body says so.

- [ ] **Step 2: Write the failing unit tests in `secrets.rs`**

```rust
#[test] fn seal_then_open_gives_the_same_text()        // open(seal("abc")) == "abc"
#[test] fn two_seals_of_one_text_differ()              // new nonce each time
#[test] fn open_with_another_api_id_fails()            // Err(SecretsError::Decrypt)
#[test] fn open_with_a_changed_byte_fails()            // flip the last byte -> Err(Decrypt)
#[test] fn open_with_an_unknown_version_fails()        // first byte 2 -> Err(UnknownVersion(2))
#[test] fn key_must_be_64_hex_characters()             // "abc" -> Err(BadKey); 64 x 'g' -> Err(BadKey)
```

- [ ] **Step 3: Implement**

  - Add `aes-gcm = "0.10"` to `crates/server/Cargo.toml`. Parse the hex key with a small local function; do not add a hex crate.
  - Sealed format: byte 0 is the version `1`, bytes 1..13 are the random nonce (`rand`), the rest is the AES-256-GCM ciphertext with the tag. The associated data is `api_id.to_be_bytes()`.
  - Migration SQL:

```sql
ALTER TABLE apis ADD COLUMN kind TEXT NOT NULL DEFAULT 'spec' CHECK (kind IN ('spec', 'manual'));
ALTER TABLE apis ALTER COLUMN spec DROP NOT NULL;
ALTER TABLE apis ADD CONSTRAINT apis_spec_matches_kind CHECK ((kind = 'spec') = (spec IS NOT NULL));
ALTER TABLE apis ADD COLUMN auth_type TEXT NOT NULL DEFAULT 'none' CHECK (auth_type IN ('none', 'api_key', 'bearer'));
ALTER TABLE apis ADD COLUMN auth_header TEXT;
ALTER TABLE apis ADD COLUMN auth_secret BYTEA;
ALTER TABLE apis ADD CONSTRAINT apis_auth_shape CHECK (
    (auth_type = 'none' AND auth_header IS NULL AND auth_secret IS NULL)
 OR (auth_type = 'bearer' AND auth_header IS NULL AND auth_secret IS NOT NULL)
 OR (auth_type = 'api_key' AND auth_header IS NOT NULL AND auth_secret IS NOT NULL));
ALTER TABLE tools ADD COLUMN source TEXT NOT NULL DEFAULT 'spec' CHECK (source IN ('spec', 'manual'));
ALTER TABLE tools ADD COLUMN edited BOOLEAN NOT NULL DEFAULT false;
-- The tool as the last import produced it. "Reset to spec" and the re-import compare against it.
ALTER TABLE tools ADD COLUMN spec_tool JSONB;
ALTER TABLE tools ADD COLUMN spec_hash TEXT;
-- A re-import finds a spec tool by its operation, so a renamed tool keeps its link.
ALTER TABLE tools ADD COLUMN operation TEXT;
UPDATE tools SET spec_tool = jsonb_build_object('name', name, 'description', description,
    'input_schema', input_schema, 'request', request),
    operation = (request->>'method') || ' ' || (request->>'path');
```

  The design stored the secret as JSON with base64. This plan uses one `BYTEA` column, because the server has no base64 crate. `spec_tool` is added so "Reset to spec" can restore the tool.
  - `config.rs`: `#[arg(long, env = "SECRETS_KEY")] pub secrets_key: String`. `AppState::new` parses it into `SecretsKey`; a bad key gives a new `StartupError::SecretsKey(#[source] SecretsError)`.
  - `.env.example`: `SECRETS_KEY=` with the comment `# openssl rand -hex 32`. `compose.yaml`: pass `SECRETS_KEY` to `server`.
  - `tests/common/mod.rs`: `config()` passes a fixed test key of 64 `0` characters.

- [ ] **Step 4: Write `tests/migrations.rs`** with `old_tool_rows_get_spec_tool_and_operation`: insert a server, a spec API, and a tool with the old columns only, then check `spec_tool->>'name'`, `operation = 'GET /pet/{petId}'`, `source = 'spec'`, and that an API with `kind = 'manual'` and a spec fails the check.

- [ ] **Step 5: Run** `cargo test -p server secrets migrations`. Expected: PASS. Run all checks.

- [ ] **Step 6: Commit** `Encrypt upstream secrets with AES-256-GCM`.

---

### Task 2: Address guard for upstream calls

**Files:**
- Create: `crates/server/src/upstream_guard.rs`
- Modify: `crates/server/src/config.rs`, `crates/server/src/state.rs`, `crates/server/src/servers.rs` (`download_spec` uses the guarded client), `compose.yaml`, `.env.example`
- Test: `crates/server/src/upstream_guard.rs` (unit), `crates/server/tests/upstream_guard.rs`

**Interfaces:**
- Produces:
  - `pub fn is_allowed(ip: IpAddr, allow_loopback: bool) -> bool`
  - `pub fn check_url(url: &Url, allow_loopback: bool) -> Result<(), BlockedAddress>` (checks IP-literal hosts and the scheme `http`/`https`).
  - `pub fn upstream_client(allow_loopback: bool) -> Result<reqwest::Client, reqwest::Error>`
  - `#[derive(Debug, thiserror::Error)] #[error("the upstream address is not allowed")] pub struct BlockedAddress;`
  - `Config.allow_local_upstream: bool` (env `ALLOW_LOCAL_UPSTREAM`, default `false`). `AppState.upstream: reqwest::Client`. `AppState.http` stays for GitHub only.

- [ ] **Step 1: Write the failing unit tests**

```rust
#[test] fn public_addresses_are_allowed()     // 1.1.1.1, 2606:4700::1111 -> true
#[test] fn private_ranges_are_blocked()       // 10.0.0.1, 172.16.0.1, 192.168.1.1, 100.64.0.1, 0.0.0.0,
                                               // 169.254.169.254, fc00::1, fe80::1, ::, 224.0.0.1 -> false
#[test] fn loopback_follows_the_setting()     // 127.0.0.1 and ::1: false with false, true with true
#[test] fn mapped_ipv6_is_checked_as_ipv4()   // ::ffff:10.0.0.1 -> false; ::ffff:127.0.0.1 with true -> true
#[test] fn the_setting_allows_only_loopback() // 10.0.0.1 with true -> false
#[test] fn check_url_refuses_ip_literals()    // http://169.254.169.254/ -> Err; http://[::1]/ with false -> Err
```

- [ ] **Step 2: Implement.** `upstream_client` sets `user_agent("toolport")`, `dns_resolver(Arc<GuardedResolver>)`, and a `redirect::Policy::custom` that stops after 10 hops and calls `check_url` on each target. `GuardedResolver` implements `reqwest::dns::Resolve` with `tokio::net::lookup_host` and drops every address that `is_allowed` refuses. When no address is left, it returns `BlockedAddress` as the error.

- [ ] **Step 3: Write `tests/upstream_guard.rs`**

  - `loopback_is_refused_without_the_setting`: `upstream_client(false)` GET to the fake upstream at `http://127.0.0.1:<port>/` fails, and the error chain contains `the upstream address is not allowed`.
  - `loopback_works_with_the_setting`: same call with `true` gives 200.
  - `name_that_resolves_to_loopback_is_refused`: GET `http://localhost:<port>/` with `false` fails.
  - `redirect_to_metadata_address_is_refused`: the fake upstream answers 302 to `http://169.254.169.254/`; with `true` the call fails with the guard error.

- [ ] **Step 4: Update `tests/common/mod.rs`** so `config()` sets `ALLOW_LOCAL_UPSTREAM=true`. Run all tests. Expected: PASS, including the old MCP tests.

- [ ] **Step 5: Commit** `Block private addresses for upstream calls`.

---

### Task 3: Input rows and the preview schema in core

**Files:**
- Create: `crates/core/src/inputs.rs`, `crates/core/tests/fixtures/preview.json`, `crates/core/tests/preview.rs`
- Modify: `crates/core/src/tool.rs`, `crates/core/src/request.rs`, `crates/core/src/lib.rs`, `crates/core/Cargo.toml` (add `ts-rs` with the same version as the server)

**Interfaces:**
- Produces:
  - `ParamDef` gets `#[serde(default, skip_serializing_if = "Option::is_none")] pub target: Option<String>`. `build_request` uses `target` as the path, query, or header key when it is set.
  - `RequestTemplate` gets `#[serde(default)] pub fixed_headers: Vec<(String, String)>`. `build_request` adds them before the input headers.
  - `pub enum InputType { String, Number, Integer, Boolean, StringArray, Complex }` (serde `snake_case`, `TS`).
  - `pub struct InputRow { pub name: String, pub input_type: InputType, pub required: bool, pub description: String }` (`TS`).
  - `pub fn schema_from_rows(rows: &[InputRow], body: Option<&Value>) -> Map<String, Value>` — only for hand-made tools; `Complex` is an error there, so the fn takes rows that the validator already checked.
  - `pub fn rows_from_schema(schema: &Map<String, Value>) -> Vec<InputRow>` — a property with `enum`, `format`, `$ref`, `oneOf`/`anyOf`/`allOf`, an `object` type, or an array of non-strings gives `Complex`.
  - `pub fn apply_rows(schema: &Map<String, Value>, rows: &[InputRow]) -> Map<String, Value>` — for spec tools: changes only `description`, the `required` list, and the `type` of non-complex properties. All other keys stay.
  - `pub fn tool_json(name: &str, description: &str, schema: &Map<String, Value>) -> Value` — gives `{"name", "description", "inputSchema"}`, the exact shape of an rmcp `Tool` in `tools/list`.
  - `pub fn parse_query_template(text: &str) -> Result<Vec<(String, String)>, TemplateError>` (pairs of query key and input name) and `pub fn print_query_template(pairs: &[(String, String)]) -> String`.

- [ ] **Step 1: Write the fixtures file.** Each case has `rows`, optional `body`, and the expected `schema`. Include: the `get_weather` case from the slide (`lat`, `lon`, both required numbers, with and without descriptions), an optional `string_array`, a case with no rows (`{"type":"object","properties":{}}`), and a body case.

- [ ] **Step 2: Write the failing tests**

```rust
#[test] fn preview_fixtures_match()                    // schema_from_rows(case.rows, case.body) == case.schema for every case
#[test] fn rows_from_schema_marks_enum_as_complex()    // {"type":"string","enum":["a"]} -> Complex
#[test] fn apply_rows_keeps_enum_and_nested_object()   // change description only; enum and nested "properties" stay byte-equal
#[test] fn apply_rows_updates_required_list()
#[test] fn query_template_round_trip()                 // "latitude={lat}&longitude={lon}" -> [("latitude","lat"),("longitude","lon")] -> same text
#[test] fn query_template_rejects_bad_text()            // "lat" , "a={}" , "a={b" -> Err
#[test] fn target_renames_the_query_key()              // ParamDef{name:"lat", target:Some("latitude")} -> query ("latitude", "47.5")
#[test] fn fixed_headers_are_sent()
#[test] fn old_request_json_still_parses()             // JSON without target and fixed_headers
#[test] fn tool_json_has_the_tools_list_shape()        // keys exactly name, description, inputSchema
```

- [ ] **Step 3: Implement** to make them pass. Keep `inputs.rs` free of I/O.

- [ ] **Step 4: Run** `cargo test -p toolport-core`. Expected: PASS. Run all checks (ts-rs writes `InputRow.ts` and `InputType.ts`).

- [ ] **Step 5: Commit** `Add input rows and the preview schema`.

---

### Task 4: One call function with an HTTP trace

**Files:**
- Create: `crates/server/src/upstream.rs`
- Modify: `crates/server/src/mcp.rs` (move `Outcome`, `send`, `read_body`, `upstream_url`, `method` into `upstream.rs`; `ToolportMcp::call` uses the new function), `crates/server/src/state.rs`
- Test: `crates/server/tests/mcp.rs`

**Interfaces:**
- Consumes: `secrets::open`, `upstream_guard::check_url`, `build_request`.
- Produces:

```rust
pub struct ApiTarget { pub id: i64, pub base_url: String, pub auth: UpstreamAuth }
pub enum UpstreamAuth { None, ApiKey { header: String, sealed: Vec<u8> }, Bearer { sealed: Vec<u8> } }
/// A secret that the Test route got in its body. It wins over the sealed one.
pub enum DraftSecret<'a> { Saved, New(&'a str) }
pub struct HttpTrace { pub request: TracedRequest, pub response: Option<TracedResponse>, pub latency_ms: i64 }
pub struct TracedRequest { pub method: String, pub url: String, pub headers: Vec<(String, String)>, pub body: Option<String> }
pub struct TracedResponse { pub status: u16, pub headers: Vec<(String, String)>, pub body: String, pub cut: bool }
pub struct CallOutput { pub result: CallToolResult, pub outcome: Outcome, pub trace: HttpTrace }
pub async fn call_upstream(ctx: &UpstreamCtx<'_>, api: &ApiTarget, secret: DraftSecret<'_>,
                           template: &RequestTemplate, args: &JsonObject) -> Result<CallOutput, CallError>
pub struct UpstreamCtx<'a> { pub http: &'a reqwest::Client, pub secrets: &'a SecretsKey, pub timeout: Duration }
```

  All `Trace*` types derive `Serialize` and `TS`. The trace replaces each secret value in the headers with `••••` (`Bearer ••••` for bearer auth). `CallError` gets `Blocked(#[from] BlockedAddress)` and `Secret(#[from] SecretsError)`. A blocked call returns a tool error result with the text `the upstream address is not allowed`, and `Outcome::Failed`.

- [ ] **Step 1: Write the failing tests in `tests/mcp.rs`**

  - `bearer_auth_reaches_upstream`: set `auth_type='bearer'` and a sealed `t0ken` on the API with SQL; the fake upstream asserts `Authorization: Bearer t0ken`.
  - `api_key_auth_reaches_upstream`: header `X-Api-Key`.
  - `blocked_base_url_gives_a_tool_error`: build the state with `ALLOW_LOCAL_UPSTREAM=false`; the call result has `isError: true` and the guard text, and the call row has `result = 'upstream_error'`.
  - Keep all old MCP tests green.

- [ ] **Step 2: Implement `call_upstream`.** It opens the secret right before `send`, and the `Secret` goes out of scope after `send`. `ToolportMcp::call` loads `t.request, a.id, a.base_url, a.auth_type, a.auth_header, a.auth_secret`, builds an `ApiTarget`, and drops the trace. `ToolportMcp::new` takes `secrets: Arc<SecretsKey>` and uses `AppState.upstream`.

- [ ] **Step 3: Write unit tests in `upstream.rs`** for the trace: `trace_masks_bearer_secret` and `trace_masks_api_key_secret` (use the fake upstream; check that no trace field contains the plain secret).

- [ ] **Step 4: Run** all tests. Expected: PASS.

- [ ] **Step 5: Commit** `Share one upstream call function with a trace`.

---

### Task 5: Manual APIs, the Test route, and `list_changed`

**Files:**
- Create: `crates/server/src/tools.rs` (routes for tools and APIs), `crates/server/src/sessions.rs`
- Modify: `crates/server/src/lib.rs`, `crates/server/src/mcp.rs` (`get_info` adds `enable_tool_list_changed()`; `list_tools` and `call_tool` register the peer), `crates/server/src/error.rs`
- Test: `crates/server/tests/tools.rs`

**Interfaces:**
- Produces:
  - `pub struct SessionPeers` in `AppState.peers: Arc<SessionPeers>`. `register(&self, server_id: i64, session_id: String, peer: Peer<RoleServer>)` (read `mcp-session-id` from the request `Parts` headers; skip when missing) and `async fn notify_tools_changed(&self, server_id: i64)` (calls `peer.notify_tool_list_changed()`; drops a peer whose send fails).
  - Types (`TS`, `#[ts(export)]`):

```rust
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthInput { None, ApiKey { header: String, #[ts(optional)] secret: Option<String> }, Bearer { #[ts(optional)] secret: Option<String> } }
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthView { None, ApiKey { header: String, secret_set: bool }, Bearer { secret_set: bool } }
pub struct ApiView { pub id: i64, pub name: String, pub kind: String, pub base_url: String, pub auth: AuthView }
pub struct SaveApi { pub name: String, pub base_url: String, pub auth: AuthInput }
pub struct ToolDraft { pub name: String, pub description: String, pub method: HttpMethod, pub path: String,
                       pub query: String, pub headers: Vec<HeaderRow>, pub inputs: Vec<InputRow>,
                       #[ts(optional)] pub body_schema: Option<serde_json::Value> }
pub struct HeaderRow { pub name: String, pub value: String }  // value "{input}" maps an input, other text is fixed
pub struct TestRequest { pub draft: ToolDraft, pub api: TestApi, pub args: serde_json::Map<String, serde_json::Value> }
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TestApi { Saved { id: i64, #[ts(optional)] auth: Option<AuthInput> }, Draft { base_url: String, auth: AuthInput } }
pub struct TestResult { pub mcp: serde_json::Value, pub http: HttpTrace }
```

  - Routes under `/api`, with `AuthUser` and `owned_server_id`:
    - `GET /servers/{name}/apis` → `Vec<ApiView>`
    - `POST /servers/{name}/apis/manual` with `SaveApi` → 201 `ApiView`
    - `PUT /servers/{name}/apis/{id}` with `SaveApi` → `ApiView`. For a spec API, a changed `name` gives 422 `spec_api_fixed`.
    - `POST /servers/{name}/tools/test` with `TestRequest` → `TestResult`. `mcp` is the serialized `CallToolResult`. Test calls are not saved and not published.
  - `pub fn draft_to_tool(draft: &ToolDraft) -> Result<ToolDef, DraftError>` in `tools.rs`. `DraftError` variants map to 422 codes: `invalid_tool_name` (MCP name rules: 1-64 chars of `[A-Za-z0-9_-]`), `unknown_placeholder` (a `{x}` in the path, query, or a header with no input row), `path_input_optional`, `bad_query_template`, `complex_input_on_manual_tool`.
  - A saved API secret is sealed in the same transaction: insert the API row, then `UPDATE apis SET auth_secret = $sealed WHERE id = $id`.

- [ ] **Step 1: Write the failing tests in `tests/tools.rs`**

  - `create_manual_api_hides_the_secret`: POST with a bearer secret; the response has `secret_set: true`; the raw response text does not contain the secret; the DB column does not contain the secret bytes.
  - `edit_api_without_secret_keeps_the_old_one`.
  - `other_users_api_is_not_found` (404).
  - `test_route_returns_mcp_result_and_masked_trace`: the fake upstream echoes the query; `mcp.isError == false`; `http.request.url` contains `latitude=47.5`; the auth header shows `Bearer ••••`.
  - `test_route_uses_saved_secret`: `TestApi::Saved` with no `auth`; upstream sees the saved token.
  - `test_route_reports_draft_errors`: `{lat}` with no input row → 422 `unknown_placeholder`.
  - `test_route_does_not_save_a_call`: the `calls` table stays empty.
  - `test_route_to_blocked_address`: state without the loopback setting → `mcp.isError == true` with the guard text.

- [ ] **Step 2: Implement** `tools.rs`, `sessions.rs`, the new `ApiError` variants, and the peer registration in `mcp.rs`.

- [ ] **Step 3: Write `list_changed_reaches_an_open_session`** in `tests/mcp.rs`: open a session, call `state.peers.notify_tools_changed(server_id)`, and read `notifications/tools/list_changed` from the session's GET stream.

- [ ] **Step 4: Run** all tests and checks. Expected: PASS.

- [ ] **Step 5: Commit** `Add manual APIs and the tool Test route`.

---

### Task 6: Create, edit, delete, and reset tools

**Files:**
- Modify: `crates/server/src/tools.rs`, `crates/server/src/servers.rs` (`ToolSummary` gets `source: String`, `edited: bool`, `spec_changed: bool`)
- Test: `crates/server/tests/tools.rs`

**Interfaces:**
- Consumes: `draft_to_tool`, `apply_rows`, `rows_from_schema`, `SessionPeers::notify_tools_changed`.
- Produces:
  - `pub struct ToolView { pub source: String, pub edited: bool, pub spec_changed: bool, pub api_id: i64, pub draft: ToolDraft, pub preview: serde_json::Value }` (`TS`). `preview` is `tool_json(..)` of the stored tool.
  - `pub struct SaveTool { pub api_id: i64, pub draft: ToolDraft }` (`TS`).
  - `pub struct CreateServerWithTool { pub server: String, pub api: SaveApi, pub draft: ToolDraft }` (`TS`).
  - Routes:
    - `GET /servers/{name}/tools/{tool}` → `ToolView`
    - `POST /servers/{name}/tools` with `SaveTool` → 201 `ToolView` (`source = 'manual'`)
    - `PUT /servers/{name}/tools/{tool}` with `SaveTool` → `ToolView`. For a spec tool: only `name`, `description`, and `inputs` count; the stored `input_schema` goes through `apply_rows`; `method`, `path`, `api_id`, `query`, `headers`, and `body_schema` must equal the stored values, or the route gives 422 `spec_tool_fixed`. Sets `edited = true`.
    - `POST /servers/{name}/tools/{tool}/reset` → `ToolView`. Copies `spec_tool` back and sets `edited = false`. 422 `not_a_spec_tool` for a manual tool.
    - `DELETE /servers/{name}/tools/{tool}` → 204. 422 `spec_tool_cannot_be_deleted` for a spec tool.
    - `POST /servers/with-tool` with `CreateServerWithTool` → 201 `ServerSummary`. One transaction: server, manual API, tool.
  - Every successful write calls `notify_tools_changed` after the commit.
  - A name clash in the server gives 409 `tool_name_taken`.

- [ ] **Step 1: Write the failing tests**

  - `create_manual_tool_appears_in_tools_list`: then an MCP `tools/list` contains the `tool_json` of the new tool, byte-equal to `ToolView.preview`.
  - `manual_tool_call_works_end_to_end`: call it over MCP with `lat`, `lon`; the upstream sees `latitude` and `longitude`.
  - `edit_spec_tool_keeps_enum`: import the Petstore spec; edit `findPetsByStatus` (its `status` input has an enum) to change only its description; the stored schema still has the enum; `edited == true`.
  - `edit_spec_tool_cannot_change_path` → 422 `spec_tool_fixed`.
  - `reset_restores_the_spec_version`.
  - `delete_spec_tool_is_refused`, `delete_manual_tool_works`.
  - `duplicate_name_is_a_conflict` → 409.
  - `create_server_with_tool_is_atomic`: an invalid draft leaves no server row.
  - `save_twice_gives_a_valid_tool` (Review Focus 5): two PUTs in a row with different descriptions; the second wins; `tools/list` shows it.

- [ ] **Step 2: Implement.**

- [ ] **Step 3: Run** all tests and checks. Expected: PASS.

- [ ] **Step 4: Commit** `Create, edit, and reset tools from the dashboard`.

---

### Task 7: Re-import keeps edited tools

**Files:**
- Modify: `crates/server/src/import.rs`, `crates/server/src/servers.rs` (`ImportedApi` gets `kept: Vec<String>` and `removed: Vec<String>`)
- Test: `crates/server/tests/import.rs`

**Interfaces:**
- Produces: `ImportReport` gets `kept: Vec<String>` (edited tools that stayed) and `removed: Vec<String>` (tools whose operation is gone).

  New re-import flow, in one transaction, for an existing API with the same name:
  1. Keep the API row (do not delete it), update `spec` and `base_url`.
  2. For each converted tool, find the old row by `operation`.
     - No old row: insert it with `source='spec'`, `spec_tool`, `spec_hash`, `operation`.
     - Old row with `edited = false`: update all tool fields, `spec_tool`, and `spec_hash`. Keep `enabled`.
     - Old row with `edited = true`: update only `spec_tool` and `spec_hash`. Keep the user's fields.
  3. Delete `source='spec'` rows of this API whose `operation` is not in the new spec.
  4. Never touch `source='manual'` rows.

  `spec_hash` is the SHA-256 hex of the canonical JSON of the converted `ToolDef` (`serde_json::to_vec` of the struct). `spec_changed` in `ToolView` is `edited AND spec_hash` differs from the hash stored at the time of the edit; store that hash in a new column `edited_hash TEXT` added by this task's migration `20261103000001_edited_hash.sql`, set by the PUT route of Task 6 (add it there in this task).

- [ ] **Step 1: Write the failing tests**

  - `reimport_keeps_edited_tool_and_its_new_name` (Review Focus 4): edit and rename `getPetById` to `pet`; re-import the same spec; `pet` exists with the edit; no `getPetById` exists.
  - `reimport_marks_spec_change`: then re-import a spec with a new summary for that operation; `ToolView.spec_changed == true`.
  - `reimport_removes_gone_operation_even_if_edited`; `report.removed` lists it.
  - `reimport_keeps_enabled_flag`.
  - `reimport_keeps_manual_tools_of_the_api`.
  - Update the old `replaced` tests to the new rule.

- [ ] **Step 2: Implement.**

- [ ] **Step 3: Run** all tests and checks. Expected: PASS.

- [ ] **Step 4: Commit** `Keep edited tools on a spec re-import`.

---

### Task 8: The builder page

**Files:**
- Create: `web/src/Builder.tsx`, `web/src/inputs.ts`, `web/scripts/check-preview.ts`
- Modify: `web/src/api.ts`, `web/src/Shell.tsx` (routes `/servers/<name>/tools/new` and `/servers/<name>/tools/<tool>`), `web/src/Live.tsx` (each tool links to the builder; a "New tool" button), `web/src/index.css`, `web/package.json` (script `check:preview`), `.github/workflows/ci.yml` (run it)

**Interfaces:**
- Consumes: the Task 5 and Task 6 routes and the generated types.
- Produces:
  - `inputs.ts`: `schemaFromRows(rows: InputRow[], body?: unknown): Record<string, unknown>`, `toolJson(name, description, schema)`, `parseQueryTemplate(text): [string, string][] | Error`, `printQueryTemplate(pairs)`. Same rules as Task 3.
  - `api.ts`: `getApis(server)`, `createManualApi(server, body: SaveApi)`, `updateApi(server, id, body)`, `getTool(server, tool)`, `createTool(server, body: SaveTool)`, `updateTool(server, tool, body)`, `resetTool(server, tool)`, `deleteTool(server, tool)`, `testTool(server, body: TestRequest)`, `createServerWithTool(body)`.

- [ ] **Step 1: Write `check-preview.ts`.** It reads `crates/core/tests/fixtures/preview.json`, runs `schemaFromRows` on each case, and exits with code 1 on the first mismatch (use `node:assert` `deepStrictEqual`). Run with `node --experimental-strip-types scripts/check-preview.ts`. Expected: FAIL (no `inputs.ts` yet).

- [ ] **Step 2: Implement `inputs.ts`.** Run `npm run check:preview`. Expected: PASS.

- [ ] **Step 3: Build `Builder.tsx`** from the slide "What you type, what Claude sees":
  - Left panel title `New tool` or `Edit tool`, with the `▷ Test` button in its header.
  - Rows with the mono labels `name`, `about`, `url` (method select + URL), `query`, `headers`, `body`, `auth`, then the inputs table (name, type select, required checkbox, description, remove button, "Add input").
  - The `url` row: an API select (existing APIs plus "New API…"). The URL text is the API base URL plus the path. Auth fields belong to the API and show the note `This auth applies to every tool of this API.` A saved secret shows as `set` with a "Replace" button.
  - Right panel `WHAT CLAUDE SEES` with the `toolJson(...)` output, pretty-printed and colored like the mockup.
  - Under both: the Test section. One field per input row for sample values. After Test: two panels `MCP result` and `Raw HTTP`.
  - A spec tool: method, URL, query, headers, body, and API are read-only; `complex` rows are read-only; a `Reset to spec` button; when `spec_changed`, the note `The spec changed this tool after your edit.`
  - Save, and Delete for a manual tool. Show `ApiError` messages in the existing `.alert` style.
  - At phone width the two panels stack.

- [ ] **Step 4: Check by hand** with `npm run dev` and the backend: build the `get_weather` tool from the slide against `api.open-meteo.com`, press Test, save it, and call it from MCP Inspector. Check all three themes.

- [ ] **Step 5: Run** `npm run lint`, `npm run build`, `npm run check:preview`, and the Rust checks. Commit `Add the tool builder page`.

---

### Task 9: Onboarding entry and the connection badge

**Files:**
- Modify: `crates/server/src/live.rs`, `crates/server/src/mcp.rs`, `web/src/Onboarding.tsx`, `web/src/liveStream.ts`, `web/src/index.css`
- Test: `crates/server/tests/mcp.rs` (or the live tests file)

**Interfaces:**
- Produces:
  - `pub struct FirstRequest { pub client_name: Option<String>, pub token_name: String, pub method: String, #[ts(optional)] pub tools_sent: Option<u32> }` (`TS`, `#[ts(export)]`).
  - `LiveHub::seen(..)` returns `bool` (true when the key is new). `list_tools` and `call_tool` publish a `client` SSE event with `FirstRequest` when it is true. `tools_sent` is set for `tools/list`.
  - `useLiveStream` exposes `firstRequest: FirstRequest | null`.

- [ ] **Step 1: Write the failing test** `first_request_sends_a_client_event`: subscribe to the live hub, send `tools/list` with a new token; the event has `method = "tools/list"` and `tools_sent = Some(n)`; a second `tools/list` sends no event.

- [ ] **Step 2: Implement** the server part. Run the tests. Expected: PASS.

- [ ] **Step 3: Onboarding.** Add the third choice `Build a tool by hand` to the pick step. It opens the builder form inside the wizard, in create-server mode. Save calls `createServerWithTool` and goes to the connect step.

- [ ] **Step 4: Badge.** Replace `FirstConnection` with one card in two states, with the exact copy from Global Constraints. WAITING shows the client command with the `Copy` button. CONNECTED uses the accent border and shows `<method> · <n> tools sent` for `tools/list`, or just `<method>` otherwise.

- [ ] **Step 5: Check by hand** the whole wizard with Claude Code (`claude mcp add --transport http ...`). Run all checks. Commit `Add the hand-made onboarding path and the badge`.

---

### Task 10: Docs

**Files:**
- Modify: `CLAUDE.md`, `.env.example`

- [ ] **Step 1:** In `CLAUDE.md`, add `aes-gcm` to the Stack list, add `SECRETS_KEY` and `ALLOW_LOCAL_UPSTREAM` to Config, mark the tool builder as done after milestone 7, and say that milestone 9 auth and the milestone 12 encryption of upstream secrets are done. Update the milestone 3 note: a re-import keeps edited tools.
- [ ] **Step 2:** Run `docker compose config`. Expected: valid. Commit `Document the tool builder`.
