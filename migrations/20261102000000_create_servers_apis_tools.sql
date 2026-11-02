-- A server is one MCP endpoint at /mcp/<name>. The URL has no user part, so the name is unique for all users.
CREATE TABLE servers (
    id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id    BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name       TEXT NOT NULL UNIQUE CHECK (name ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX servers_user_id_idx ON servers (user_id);

-- The spec is TEXT and not JSONB, because JSONB changes the key order and a re-import compares specs.
CREATE TABLE apis (
    id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    server_id  BIGINT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    base_url   TEXT NOT NULL,
    spec       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (server_id, name),
    UNIQUE (id, server_id)
);

-- server_id is a copy of the API's server, so the database can keep tool names unique per server.
-- The two-column foreign key makes sure that the copy matches the API.
CREATE TABLE tools (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    api_id       BIGINT NOT NULL,
    server_id    BIGINT NOT NULL,
    name         TEXT NOT NULL,
    description  TEXT NOT NULL,
    input_schema JSONB NOT NULL,
    request      JSONB NOT NULL,
    enabled      BOOLEAN NOT NULL DEFAULT true,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (api_id, server_id) REFERENCES apis (id, server_id) ON DELETE CASCADE,
    UNIQUE (server_id, name)
);

CREATE INDEX tools_api_id_idx ON tools (api_id);
