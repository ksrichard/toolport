-- NULL means that the token never expires. It stays valid until a revoke.
ALTER TABLE mcp_tokens ALTER COLUMN expires_at DROP NOT NULL;
