-- The call cleanup deletes old rows of all servers. The index on (server_id, created_at)
-- cannot find them without a full scan, so the cleanup needs an index on created_at alone.
CREATE INDEX calls_created_at_idx ON calls (created_at);
