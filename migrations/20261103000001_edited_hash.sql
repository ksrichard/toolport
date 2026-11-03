-- The spec_hash that was current when the user saved an edit. A different spec_hash later
-- means the spec changed under the edit.
ALTER TABLE tools ADD COLUMN edited_hash TEXT;
