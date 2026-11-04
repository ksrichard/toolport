-- A revoke now deletes the token row. The call log keeps the token name in its own column,
-- because `calls.token_id` becomes NULL when the token row goes away.
ALTER TABLE calls ADD COLUMN token_name TEXT;
UPDATE calls c SET token_name = t.name FROM mcp_tokens t WHERE t.id = c.token_id;

DELETE FROM mcp_tokens WHERE revoked_at IS NOT NULL;
ALTER TABLE mcp_tokens DROP COLUMN revoked_at;
