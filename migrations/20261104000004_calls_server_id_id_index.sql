-- The call log pages by id inside one server. This index finds a page without a scan of
-- the calls of other servers.
CREATE INDEX calls_server_id_id_idx ON calls (server_id, id);
