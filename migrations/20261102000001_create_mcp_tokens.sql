-- The token value is a JWT, and the database never stores it. Only the jti is here.
-- A token is valid only while its row exists and is not revoked, so a revoke works at once.
CREATE TABLE mcp_tokens (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    server_id    BIGINT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    jti          TEXT NOT NULL UNIQUE,
    name         TEXT NOT NULL CHECK (name <> ''),
    expires_at   TIMESTAMPTZ NOT NULL,
    last_used_at TIMESTAMPTZ,
    revoked_at   TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX mcp_tokens_server_id_idx ON mcp_tokens (server_id);
