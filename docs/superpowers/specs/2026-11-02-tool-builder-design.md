# Tool builder: design

Date: 2026-11-02. Status: draft, not approved.

## Intent

Claude only sees a tool's name, its description, and its input schema. The tool builder
shows the user exactly that, as they type. It also has a Test button. Test runs the tool
through the real call code and shows the result as Claude gets it, next to the raw HTTP
request and response.

The builder is the editor for every tool:
- a new tool that the user builds by hand,
- a tool from an imported spec,
- a tool from a template.

So a user can build a whole server by hand, add one hand-made tool to a server with
imported tools, or fix a weak description of an imported tool.

Design source: three slides from the user ("Tool builder", "What you type, what Claude
sees", "The badge flips on the first request"). Keep their layout and text. Use the shell,
panels, and CSS tokens of `mockups/toolport-mockups.html` and all three themes.

## Goals

1. The builder page: a form on the left, the "What Claude sees" panel on the right.
   The panel shows the exact JSON object that `tools/list` sends for this tool. It updates
   on each key press.
2. The form supports:
   - name and description ("about"),
   - method and URL (all methods in `HttpMethod`),
   - path values (`/repos/{owner}/{repo}`) and query values (`latitude={lat}&longitude={lon}`),
   - fixed extra headers and header values from inputs,
   - a JSON request body,
   - upstream auth: none, API key in a header, or bearer token.
3. Inputs are a table with one row per input: name, type, required, description.
4. Test: the user types sample values for the inputs and clicks Test. ToolPort runs the
   draft tool through the same code as a real MCP call. The page shows two panels:
   - "MCP result": the `CallToolResult` as Claude gets it (content text and `isError`).
   - "Raw HTTP": the request (method, full URL, headers, body) and the response
     (status, headers, body, latency). Secret values are masked, for example
     `Authorization: Bearer ••••`.
5. Save creates or updates the tool. Saving sends `notifications/tools/list_changed` to the
   open MCP sessions of that server.
6. Entry points:
   - Onboarding: a third choice "Build a tool by hand", next to "Pick a template" and
     "Own spec". The user builds the first tool, names the server, and goes on to the
     connect step.
   - A server with tools: "New tool" and "Edit" on each tool. Until the Tools page
     (milestone 8) exists, the Live page tool list links to the builder.
7. Upstream secrets: every user secret for upstream HTTP calls is encrypted at rest with
   AES-256-GCM. See "Secrets".
8. Onboarding badge (third slide): the connect step shows one card that changes from
   WAITING to CONNECTED on the first MCP request with the new token. The CONNECTED card
   names the client and shows the first request, for example `tools/list · 3 tools sent`.
   Newer clients skip the old `initialize` handshake, so any MCP request counts.

## Non-goals

- YAML specs, OAuth upstream auth, or non-JSON request bodies.
- Response transforms. Claude gets the upstream body as today.
- A version history of tool edits.
- The full Tools page (milestone 8) and the full APIs page (milestone 9). This work adds
  only what the builder needs from them.

## Anti-goals

- The preview must never differ from what `tools/list` really sends. The preview and the
  server must use one definition of the tool JSON (see "One source of truth").
- No secret value may reach the browser, the logs, the call log, or a Test result after
  it is saved.
- An edit must not get lost silently on a re-import.

## Constraints

- Stack and rules from `CLAUDE.md`: typed `thiserror` errors, `ApiError` responses,
  `#[sqlx::test]` with a real DB, no mocks except upstream APIs and GitHub, ts-rs types.
- The user approved one new crate: `aes-gcm`.
- Existing data must keep working: imported tools, template tools, and their `request` JSON.

## Decided by the user

- Both flows: a whole server by hand, and one hand-made tool next to imported tools.
- The builder edits existing tools too (imported, template, and hand-made).
- Test shows both the MCP result and the raw HTTP request and response.
- The form supports GET and POST, query and path values, a JSON body, extra headers,
  and upstream auth.
- Inputs are a table with one row per input.
- Upstream secrets use AES-256-GCM. ToolPort decrypts a secret only in memory, just
  before the upstream request.

## Data model

### APIs and auth

Auth belongs to the API, because all tools of one upstream service share it.
Add to `apis`:
- `kind TEXT NOT NULL DEFAULT 'spec'`, with `CHECK (kind IN ('spec', 'manual'))`.
- `spec` becomes nullable. A `manual` API has no spec.
- `auth JSONB NOT NULL DEFAULT '{"type":"none"}'`. Shapes:
  - `{"type":"none"}`
  - `{"type":"api_key","header":"X-Api-Key","secret":<sealed>}`
  - `{"type":"bearer","secret":<sealed>}`

A hand-made tool belongs to an API. In the builder the user picks an existing API of the
server or creates a new manual API (name, base URL, auth). The URL field shows the base URL
plus the path. Changing the auth of an API changes it for all tools of that API. The
builder says so next to the auth fields.

### Tools

Add to `tools`:
- `source TEXT NOT NULL DEFAULT 'spec'`, `CHECK (source IN ('spec', 'manual'))`.
- `edited BOOLEAN NOT NULL DEFAULT false`. True when the user saved a spec tool in the builder.
- `spec_hash TEXT NULL`. The SHA-256 of the tool definition that the last import produced.

Extend `RequestTemplate` (stored as JSON in `tools.request`):
- `ParamDef` gets an optional `target` name. The input `lat` can then fill the query key
  `latitude`. When `target` is absent, the key is the input name, as today. Old rows stay valid.
- `fixed_headers: Vec<(String, String)>` with `#[serde(default)]`, for headers without an input.
- Keep `path`, `params`, and `body` as they are.

The query template text in the form (`latitude={lat}&longitude={lon}`) is only a view of
the query `ParamDef`s. The page parses it into params and prints params back into it.

## Secrets

- New env var `SECRETS_KEY`: 32 bytes as 64 hex characters (`openssl rand -hex 32`). It is
  a `clap` setting like the others. The server and both dev binaries refuse to start
  without a valid key. Add it to `.env.example` and `compose.yaml`.
- A sealed secret is stored as `{"v":1,"nonce":"<base64>","ct":"<base64>"}`. Each value
  gets a new random 12-byte nonce. The associated data is the API id, so a sealed value
  copied to another API row does not decrypt.
- A `secrets` module in `crates/server` owns seal and open. The open result is a type
  that does not implement `Debug` or `Display` and that zeroes its memory on drop, so it
  cannot reach a log by accident.
- ToolPort opens the secret right before it builds the upstream request, and drops it
  after the request.
- The dashboard API never returns a secret. It returns `{"type":"bearer","secret_set":true}`.
  A save without a new secret keeps the old one.

## Call code

Today `mcp.rs` builds and sends the upstream request. Move this into one function that
takes a tool definition, the API (base URL and auth), and the arguments, and returns:
- the `CallToolResult` for Claude,
- a trace of the HTTP request and response for the Test panel.

The MCP endpoint and the Test endpoint both call it. The MCP endpoint drops the trace.

## REST API (dashboard)

All routes need the Bearer JWT and check that the user owns the server.

- `GET /api/servers/{name}/tools/{tool}`: the full editable definition, including the
  input rows and the API with `secret_set` flags.
- `POST /api/servers/{name}/tools`: create a hand-made tool.
- `PUT /api/servers/{name}/tools/{tool}`: save an edit. For a spec tool, this sets `edited`.
- `DELETE /api/servers/{name}/tools/{tool}`: delete a hand-made tool. A spec tool can only
  be turned off, because a re-import would bring it back.
- `POST /api/servers/{name}/tools/test`: run a draft. The body holds the draft tool, the
  API (an id, or a draft API with an optional new secret), and the sample arguments.
  A secret in this body is used in memory only and is never saved or logged.
- `POST /api/servers/{name}/apis` and `PUT /api/servers/{name}/apis/{api}`: create and
  edit a manual API (name, base URL, auth).
- The onboarding uses a create call that makes the server, the manual API, and the first
  tool in one transaction.

Validation on save: the tool name matches the MCP tool name rules and is unique in the
server. Every `{placeholder}` in the path and in the query template has a matching input.
Every path input is required. At least one tool of the server stays on (milestone 8 rule).

## One source of truth for the preview

The "What Claude sees" JSON is `{name, description, inputSchema}`. The input schema comes
from the input rows: `type: object`, one property per row (`type`, and `description`
when set), and `required` with the required rows. A JSON body adds the `body` property as
today.

The server builds this with a function in `toolport-core`. The page builds it with a
TypeScript function. A shared test fixture file holds pairs of input rows and expected
JSON. A Rust test and a TypeScript check both run the fixtures, so the two functions cannot
drift. ts-rs generates the types of the input rows.

## Re-import

Today a re-import replaces all tools of the API. New rule:
- A tool with `edited = false` is replaced as today.
- A tool with `edited = true` keeps the user's version. If the spec changed this
  operation (the new hash differs from `spec_hash`), the builder shows a note:
  "The spec changed this tool after your edit." with a "Reset to spec" button.
- An operation that is gone from the spec removes its tool, even when edited. The import
  report lists it.

## Live updates

- Saving, deleting, or switching a tool sends `notifications/tools/list_changed` to the
  open sessions of the server.
- The SSE stream gets a `client` event on the first MCP request of a token and client
  name in a session window. It holds the client name, the MCP method, and for
  `tools/list` the number of tools sent. The onboarding badge uses it.

## Pages

- Builder page, route `/servers/{name}/tools/new` and `/servers/{name}/tools/{tool}`.
  Left: the "New tool" (or "Edit tool") panel with the Test button in its header, the
  fields `name`, `about`, `url` (method select plus URL), `query`, `headers`, `body`,
  `auth`, and the inputs table. Right: "WHAT CLAUDE SEES". Under them: the Test panel
  with sample values, then "MCP result" and "Raw HTTP".
- A spec tool opens in the same form. Its method, path, and API are read-only, because
  they come from the spec. Name, description, input descriptions, and the required flag
  of query and header inputs are editable.
- Onboarding: the third choice and the WAITING / CONNECTED badge card.
- At phone width the two panels stack.

## Tests

- Core: input rows to schema, query template parse and print, `target` names in
  `build_request`, the shared preview fixtures.
- Server (`#[sqlx::test]`, upstream mocked): create, edit, delete, validation errors,
  ownership, Test result and masked trace, secret round trip, a sealed secret never
  appears in any API response, re-import with edited tools, `list_changed` on save.
- Migration test: old `request` rows still load and still call correctly.

## Blocked upstream addresses

Decided by the user. Every upstream call (Test and real MCP calls) refuses private,
loopback, link-local, and other non-public addresses. Examples: `10.0.0.0/8`,
`172.16.0.0/12`, `192.168.0.0/16`, `127.0.0.0/8`, `169.254.0.0/16` (cloud metadata),
`::1`, `fc00::/7`, `fe80::/10`, and IPv4-mapped forms of these.

- The check runs on the resolved IP addresses, inside a custom DNS resolver of the
  `reqwest` client. A check on the URL text alone is not enough, because a public name
  can resolve to a private address. The resolver also covers every redirect.
- A blocked call returns a clear tool error to Claude ("the upstream address is not
  allowed") and the Test panel shows the same text. It is saved in the call log as an
  upstream error. My call: it adds no new `CallResult` variant.
- Dev setting `--allow-local-upstream` / `ALLOW_LOCAL_UPSTREAM`, default `false`.
  When it is `true`, loopback addresses (`127.0.0.0/8`, `::1`) are allowed. All other
  blocked ranges stay blocked. Compose leaves it `false`.
- Tests that call a mock upstream on `127.0.0.1` turn the setting on. One test checks
  that a loopback URL is refused when the setting is off.

## Answers to the builder check

- Spec tool schemas: the stored `input_schema` stays the source of truth. The input rows
  are a view of its top-level properties. A save changes only the fields that the form
  edits (description, the required flag, the type of a simple property). A property with
  detail that the table cannot show (enum, format, nested object, array of objects) keeps
  that detail. The table shows such a row with a "complex" type that is read-only.
  For a hand-made tool the rows are the source and the schema is built from them.
- `list_changed`: the MCP service already runs stateful sessions (`LocalSessionManager`).
  The server keeps a map from server id to the open session peers and sends the
  notification to each. It is best effort. A failed send drops that peer from the map.
- Re-import matching: a spec tool is matched to its operation by method and path, not by
  name, so an edited tool can have a new name. `edited` covers the whole tool. "Reset to
  spec" restores the tool as the last import produced it, and clears `edited`.
- Mixed APIs: a hand-made tool can belong to a spec API. A re-import touches only the
  tools with `source = 'spec'`.
- Auth on spec APIs: yes. The API edit endpoint accepts both kinds, but for a spec API only
  the base URL and the auth can change. This is the auth part of milestone 9.
- `SECRETS_KEY`: only the `server` binary needs it. `import-spec` and `mcp-token` do not
  handle upstream secrets. No key rotation now. The `v` field in the sealed format allows
  it later. Tests use a fixed test key.
- The associated data needs the API id, so a new API is inserted first and its sealed
  secret is written in a second statement in the same transaction.
- Test with an existing API and no new secret uses the saved secret.

## Left to the builder (my calls, not the user's)

- The env var name `SECRETS_KEY` and the sealed value format.
- Auth on the API, not on the tool.
- Test calls do not go into the call log and do not count in the metrics.
- The exact type list for inputs: string, number, integer, boolean, array of strings.
- The page routes and the read-only fields of a spec tool.
- The split into PRs. Suggested order: (1) commit milestone 7 as it is, (2) secrets and
  API auth, (3) call code refactor with trace, (4) tool REST API and Test, (5) builder page,
  (6) onboarding entry and badge, (7) re-import rule.
