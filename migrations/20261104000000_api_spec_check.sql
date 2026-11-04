-- The URL that the spec came from. It is NULL for a spec from a file or text and for manual APIs.
ALTER TABLE apis ADD COLUMN spec_url TEXT;
-- The daily check downloads the spec again and re-imports it when the text changed.
ALTER TABLE apis ADD COLUMN auto_check BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE apis ADD COLUMN last_check_at TIMESTAMPTZ;
ALTER TABLE apis ADD COLUMN last_check_result TEXT
    CHECK (last_check_result IN ('unchanged', 'updated', 'failed'));
ALTER TABLE apis ADD CONSTRAINT apis_auto_check_needs_url CHECK (NOT auto_check OR spec_url IS NOT NULL);
