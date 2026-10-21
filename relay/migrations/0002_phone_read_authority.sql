-- Existing phones must re-pair after this migration and the matching application update.
-- Legacy rows intentionally cannot authenticate snapshot reads without a phone read hash.
ALTER TABLE devices ADD COLUMN reader_hash TEXT;
