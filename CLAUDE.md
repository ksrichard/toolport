# ToolPort

ToolPort turns REST APIs into MCP servers. A user imports an OpenAPI spec. ToolPort then
serves each API operation as an MCP tool at `https://<host>/mcp/<server-name>`. AI clients
such as Claude Desktop and Cursor connect to that URL with a token and call the tools.
A web dashboard shows the live traffic and lets the user control tools, tokens, and servers.

This is a personal course project. Do not use the shared company skills folder. Do not
name that company anywhere in this repo.

## Design source
`mockups/toolport-mockups.html` is the design for the Live page. Keep its layout, text,
and the three themes (Neon terminal = default, Calm dark, Clean light). The other pages
use the same shell, panels, and tokens.

## Stack
- Backend: Rust (stable, edition 2024). Use these crates for these jobs:
  - `tokio`: the async runtime.
  - `axum`: the HTTP server and routing.
  - `thiserror`: typed error enums everywhere, including `main` (see "Errors" below).
  - `anyhow`: only in throwaway scripts or dev tools. Do not use it in `toolport-core` or `server`.
  - `clap` (derive + env): the server settings. Each setting is a flag and an env var.
- MCP: `rmcp` (official Rust MCP SDK), Streamable HTTP transport, mounted in axum.
- OpenAPI parsing: `openapiv3` (3.0) with serde. Reject 2.0 (Swagger) specs with a clear error.
- Upstream HTTP calls and GitHub calls: `reqwest` (rustls). The upstream client refuses private,
  loopback, link-local, and other non-public addresses. This covers DNS answers, IP-literal URLs,
  and redirects. It ignores `HTTP_PROXY`, `HTTPS_PROXY`, and `ALL_PROXY` on purpose, because a
  proxy would skip the address check.
- Upstream secrets at rest: `aes-gcm` (AES-256-GCM) encrypts them. `zeroize` wipes decrypted
  secrets from memory.
- Database: PostgreSQL through `sqlx` with migrations. Local dev runs Postgres in Docker Compose.
- Auth helpers: `jsonwebtoken` (JWT), `axum-extra` + `cookie` (the refresh cookie),
  `rand` (random tokens), `sha2` (token hashes).
- Dashboard login: GitHub OAuth only (no passwords). Add other login methods later only if needed.
- Authorization: JWT (HS256, secret from `JWT_SECRET`). See "Auth" below.
- MCP clients (milestone 4): each token is a JWT with the server id, a unique `jti`, and its
  own audience. The server checks that the `jti` row exists and is not revoked, so revoke
  works at once. The token value shows once, at creation. The DB stores only the `jti`.
  Each token has a lifetime of 1 to 365 days (default 90) or never. A token that never expires has no `exp`.
- Logs: `tracing` with JSON output.
- Frontend: React + Vite + TypeScript. Plain CSS with the mockup's CSS variables. No UI kit.
  `@formkit/auto-animate` animates items that lists add, remove, and move. It is small and has no
  dependencies. All other animations are plain CSS.
- Shared types: `ts-rs` generates TypeScript types from the Rust API types.
  The preview rules have a shared fixture, `crates/core/tests/fixtures/preview.json`. A Rust test
  and `npm run check:preview` both check it.
- Live updates to the dashboard: Server-Sent Events (SSE).

## Repo layout
    Cargo.toml            Cargo workspace
    crates/core           package `toolport-core`: domain types, OpenAPI -> tool conversion, no I/O
                          (the name `core` clashes with Rust's built-in crate)
    crates/server         axum app: REST API for the dashboard, MCP endpoint, SSE, DB
    web/                  React app (Vite)
    migrations/           sqlx migrations
    docker/               Dockerfiles and the Caddyfile
    compose.yaml          Docker Compose file for the whole system
    .env.example          all env vars with safe example values (no real secrets)
    mockups/              design files

## Auth
Code: `crates/server/src/auth/`, `web/src/api.ts`.
- Every `/api/*` request sends the access JWT as `Authorization: Bearer <jwt>`. The JWT
  lives 15 minutes, and its audience is `toolport-dashboard`. The frontend keeps it in memory only.
- The refresh token is the only long-lived token. It is 32 random bytes, and it lives
  30 days in an HttpOnly, Secure, SameSite=Strict cookie with `Path=/auth`. The browser sends
  it only to `/auth/*`. The DB keeps only its SHA-256 hash.
- Each refresh rotates the token. A used or revoked refresh token gives 401.
- Flow:
  1. `GET /auth/github/login` sets a short-lived `state` cookie and redirects to GitHub.
  2. `GET /auth/github/callback` checks `state`, gets the GitHub user, creates or updates the
     user row, sets the refresh cookie, and redirects to `/`. A failure redirects to
     `/?login_error=<code>`.
  3. The app calls `POST /auth/refresh` and gets `{access_token, token_type, expires_in}`.
  4. When an API call returns 401, the app refreshes once and retries.
  5. `POST /auth/logout` revokes the refresh token and clears the cookie.
- Do not put the access JWT in a cookie, `localStorage`, or a URL.

GitHub OAuth app (one-time setup, https://github.com/settings/developers):
- Homepage URL: `https://localhost`
- Callback URL: `https://localhost/auth/github/callback` (Docker Compose)
- A GitHub OAuth app allows only one callback host and port. For the Vite dev server, make a
  second app with `http://localhost:5173/auth/github/callback`, and use its ID and secret on the host.

## Docker setup
One command starts the whole system: `docker compose up --build`.

Services in `compose.yaml`:
- `postgres`: the `postgres:17` image. A named volume keeps the data. A health check uses `pg_isready`.
- `server`: the Rust backend. It starts after `postgres` is healthy. It runs the sqlx
  migrations at startup. It listens on port 8080 inside the Docker network only.
- `caddy`: the HTTP proxy and the only service with public ports (80 and 443).
  It serves the built React app and sends API traffic to `server`.

Images:
- `docker/server.Dockerfile`: a multi-stage build. Stage 1 uses `rust:1-bookworm` and
  `cargo-chef` to cache dependencies. Stage 2 is `debian:bookworm-slim` with only the
  binary and CA certificates. The process runs as a non-root user.
- `docker/caddy.Dockerfile`: stage 1 uses `node:22` to run `npm ci && npm run build` in `web/`.
  Stage 2 is the `caddy:2` image with the `dist/` folder and the Caddyfile.

Caddy routes (`docker/Caddyfile`):
- `/api/*`, `/auth/*`, `/mcp/*` go to `server:8080`.
- Every other path serves the React app, with a fallback to `index.html`.
- The SSE and MCP routes use `flush_interval -1`, so events reach the client at once.
- The site address comes from the `TOOLPORT_DOMAIN` env var. The default is `localhost`,
  and Caddy uses its own local TLS certificate for it. With a real domain, Caddy gets a
  Let's Encrypt certificate.

Config:
- All secrets come from `.env` (not committed): `DATABASE_URL`, `JWT_SECRET`,
  `SECRETS_KEY`, `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET`, `TOOLPORT_DOMAIN`, `POSTGRES_PASSWORD`, `PUBLIC_URL`.
- Compose sets the server's `PUBLIC_URL` to `https://$TOOLPORT_DOMAIN`. The `PUBLIC_URL` in
  `.env` is for host runs only.
- `ALLOW_LOCAL_UPSTREAM` (default false) allows upstream calls to loopback addresses only. Use it
  for local dev and tests.
- `SPEC_CHECK_INTERVAL_SECS` (default 3600) is the time between two runs of the spec check.
  Each run checks the APIs whose daily check is on and whose last check is older than 24 hours.
- `.env.example` lists every variable. CI checks that the Compose file is valid
  (`docker compose config`).

Local development without Docker for the app code: run only Postgres with
`docker compose up -d postgres`, then run the backend and Vite on the host.
Vite proxies `/api`, `/auth`, and `/mcp` to `localhost:8080`.

## Errors
- Every fallible function returns a typed `thiserror` enum. Do not return `anyhow::Error`,
  `Box<dyn Error>`, or `String` errors.
- Each module or area has its own enum (for example `StartupError`, `ApiError`). Keep the
  underlying error as `#[source]` or `#[from]`, so the cause chain stays.
- Error messages are lowercase and have no final period.
- HTTP handlers return `Result<_, ApiError>`. `ApiError` maps each variant to one HTTP status
  and one stable `snake_case` code. The response body is always
  `{"error": "<code>", "message": "<text>"}`. The client never sees the inner cause.
- `ApiError` logs the full cause chain with `error_chain()` (`crates/server/src/error.rs`).
- `main` calls `run() -> Result<(), StartupError>`, logs the chain, and exits with code 1.
- Do not use `unwrap()` or `expect()` outside tests.

## Main concepts
- User: a person who logs in to the dashboard.
- Server: one MCP endpoint (for example `petstore-prod`). A user owns many servers.
- API: one imported OpenAPI spec with its base URL and upstream auth. A server has many APIs.
- Tool: one API operation. It has a name (operationId), an input JSON schema, and an on/off flag.
- Token: a secret that an MCP client uses to connect to one server.
- Call: one tool call. It stores the time, tool, token, HTTP status, latency, and the result
  (OK, Not found, Upstream error, Timed out).

## Milestones
Each milestone ends with working code, tests, and a short demo.

1. Setup. Create the Cargo workspace, the Vite app, and the first migration. Add the two
   Dockerfiles, the Caddyfile, `compose.yaml`, and `.env.example`. `docker compose up --build`
   must show an empty React page at `https://localhost` and a health answer at `/api/health`.
   Add CI that runs `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, the web
   build, `docker compose config`, and both image builds.
2. OpenAPI to tools. Parse a spec and make one tool per operation. Merge path, query, and
   body parameters into one input schema. Test with the Petstore spec.
   Status: done. Limits: JSON specs only (YAML needs a new crate). Only JSON request bodies;
   an operation with a required non-JSON body is skipped, an optional one loses its body.
3. MCP endpoint. Serve the tools of one server at `/mcp/<server>` with `rmcp`. Turn each
   tool call into an HTTP request to the upstream API and return the response. Add a
   timeout (10 s default). Check it with MCP Inspector.
   Status: done. Until the APIs and servers pages exist, the `import-spec` binary loads a spec
   into a server. A re-import of an API matches tools by method and path.
   It keeps edited tools and the `enabled` flag, and removes tools whose operation is gone.
4. Tokens. Make MCP clients send a JWT as a bearer token. Check the signature, the expiry,
   and the `jti` row. Show the token value once, at creation. Allow revoke.
   Status: done. The `mcp-token` dev binary also creates, lists, and revokes tokens. A missing token, a bad token, and an unknown server all get the same 401.
5. Call log and metrics. Save every call. Compute calls per minute, error rate, p50 and
   p95 latency, and open sessions. Send new calls and metrics to the dashboard over SSE.
   Status: done. `GET /api/servers/{name}/live` sends `snapshot`, `call`, and `metrics`
   events. It needs the Bearer JWT, so the dashboard reads it with `fetch`, not `EventSource`.
   A "connected client" is a token and client name with an MCP request in the last 2 minutes.
   Only calls that reach the upstream API are saved.
6. Users and dashboard shell. Add sign-in with GitHub (OAuth) and logout. The first
   GitHub sign-in creates the user. The server then sets the refresh cookie, and the app gets
   a Bearer JWT from `/auth/refresh`. Build the side menu, the header with the MCP URL and the
   Copy button, and the theme switcher (saved per user).
   Status: done. The Live page shows the MCP URL and the Copy button.
7. Live page. Build it from the mockup with real data from milestone 5. Keep the Pause button.
   Status: done, with onboarding. A user with no server gets a wizard: pick a template
   (`crates/server/templates/`) or an own spec, name the server, copy the client command.
   The tool switches stay read-only until milestone 8. `cargo test` writes the API types to
   `web/src/types/` (ts-rs), and CI fails when they are out of date.
   In the Neon theme, `web/src/PixelScenes.tsx` plays a short pixel-art scene in the free space of
   the page header every 30–60 s. `?pixels=demo` plays them back to back. The × button turns the scenes off for this browser.
   Tool builder. Status: done. Pages: `/servers/<name>/tools/new` and
   `/servers/<name>/tools/<tool>`. The builder edits hand-made, template, and imported tools.
   The preview is the exact `tools/list` JSON. Test shows the MCP result and the raw HTTP, with
   secrets masked as `••••`. It uses `POST /api/servers/{name}/tool-test`, or `POST /api/tool-test`
   for a draft API in onboarding. Onboarding has "Build a tool by hand" and the
   WAITING/CONNECTED badge. Saving a tool sends `notifications/tools/list_changed`.
   An imported tool locks the fields that a re-import matches on. "Detach from spec" turns it
   into a hand-made tool, and a re-import then leaves it alone.
8. Tools page. List all tools with on/off switches. When a tool changes, send
   `notifications/tools/list_changed` to the open MCP sessions. Keep at least one tool on.
   Status: done. `PUT /api/servers/{name}/tools/{tool}/enabled` turns a tool on or off. The Live
   page and the Tools page use the same switch, and the server refuses to turn off the last tool.
9. APIs page. Add an API from a spec URL or a file. Set the base URL and the upstream auth
   (none, API key header, or bearer). Re-import a spec and show which tools were added or removed.
   Add a "Re-import" button for each API. It downloads the spec again from its URL, or asks for
   a new file when the API came from a file. It shows the report: added, kept (edited), and
   removed tools.
   Add an optional daily check for each API from a URL. The server downloads the spec once a day.
   When the spec changed, it re-imports with the same rule and sends `list_changed`. The APIs page
   shows the time and result of the last check. The check is off by default.
   Status: done. The spec URL is stored only for a URL import. A re-import from a file removes
   the URL and turns the check off. The check runs in the server process, so tests call
   `check_due_apis` directly. It records `unchanged`, `updated`, or `failed` for each API.
   `DELETE /api/servers/{name}/apis/{id}` removes an API and its tools. The call log stays.
   The server refuses with `last_enabled_tool` when no enabled tool would remain.
10. Tokens page. Create, name, and revoke tokens. Show the last use of each token.
    Status: done. `GET`/`POST /api/servers/{name}/tokens` list and create tokens, and
    `POST /api/servers/{name}/tokens/{id}/revoke` revokes one. Revoked tokens stay in the list.
    The last use updates at most once a minute. The onboarding badge matches its token by id.
    The Create form offers a lifetime select, and the table shows the expiry date, "never", or "expired".
11. All servers page. Create, rename, and delete servers. Show a short status for each.
    Status: done. `PUT /api/servers/{name}` renames a server, and `DELETE /api/servers/{name}` deletes
    it with its APIs, tools, tokens, and calls. Both close the open MCP sessions of the server. After a
    rename the tokens stay valid, and the old URL gives the same 401 as an unknown server. The name
    `with-tool` is reserved, because `POST /api/servers/with-tool` is a fixed route.
12. Hardening. Encrypt upstream secrets at rest. Add rate limits per token. Delete old call
    rows after a set time.
    Status: the encryption of upstream secrets is done. Rate limits and call-row cleanup are not.
13. Deploy. Run the same `compose.yaml` on one host with a real `TOOLPORT_DOMAIN`.
    Add a Postgres backup job. Ask before any change to real infrastructure.

## Rules for work in this repo
- Write all text (docs, comments, PRs, commits) in simple technical English.
  One idea per sentence. Active voice. No metaphors.
- Keep comments few. Explain why, not what.
- Only mock external systems in tests (the upstream APIs and GitHub). Use a real Postgres DB
  in tests (`#[sqlx::test]`).
- Never store or log secrets in plain text. Never print token values after creation.
- Do not add dummy or seed data unless asked.
- main is PR-only. Use `feature/...`, `fix/...`, `refactor/...` branches. Keep PRs small.
- Ask before you add a new dependency that is not in the Stack list.

## Commands
- First time: `cp .env.example .env`. Set `POSTGRES_PASSWORD` and the same password in
  `DATABASE_URL`. Set `JWT_SECRET` and `SECRETS_KEY` (both `openssl rand -hex 32`; `SECRETS_KEY` must be 64 hex characters) and the GitHub OAuth app values.
- Whole system: `docker compose up --build`, then open `https://localhost`.
  Your browser warns about the local Caddy certificate the first time.
- Database only: `docker compose up -d --wait postgres`
- Load the env vars into the shell: `set -a; . ./.env; set +a`
- Backend: `cargo run -p server` (`-- --help` lists the settings)
- Import a spec into a server (dev tool): `cargo run -p server --bin import-spec -- --owner <github-login>
  --server <name> --spec <file-or-url> [--base-url <url>]`. The owner must have signed in once.
- MCP tokens (dev tool): `cargo run -p server --bin mcp-token -- create --server <name> --name <label> [--days <n> | --never]`,
  also `list --server <name>` and `revoke --id <id>`. It needs `JWT_SECRET` from `.env`.
- Checks: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`
  (the tests need Postgres and `DATABASE_URL`)
- Frontend: `cd web && npm run dev` (Vite sends `/api`, `/auth`, `/mcp` to `localhost:8080`),
  `npm run lint`, `npm run build`, `npm run check:preview`
- CI: `.github/workflows/ci.yml` runs all the checks above, `docker compose config`, and both image builds.
