-- Manual APIs have no spec. The constraint keeps kind and spec in step.
ALTER TABLE apis ADD COLUMN kind TEXT NOT NULL DEFAULT 'spec' CHECK (kind IN ('spec', 'manual'));
ALTER TABLE apis ALTER COLUMN spec DROP NOT NULL;
ALTER TABLE apis ADD CONSTRAINT apis_spec_matches_kind CHECK ((kind = 'spec') = (spec IS NOT NULL));

-- auth_secret holds the sealed secret (see secrets.rs), never the plain text.
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
