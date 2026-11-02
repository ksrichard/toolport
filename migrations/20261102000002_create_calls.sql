-- One row per MCP tool call that reached the upstream API. The Live page reads it.
-- `tool` is a name and not a foreign key, because a re-import replaces the tool rows.
CREATE TABLE calls (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    server_id   BIGINT NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    token_id    BIGINT REFERENCES mcp_tokens (id) ON DELETE SET NULL,
    client_name TEXT,
    tool        TEXT NOT NULL,
    result      TEXT NOT NULL CHECK (result IN ('ok', 'not_found', 'upstream_error', 'timed_out')),
    http_status SMALLINT,
    latency_ms  INTEGER NOT NULL CHECK (latency_ms >= 0),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX calls_server_id_created_at_idx ON calls (server_id, created_at);
