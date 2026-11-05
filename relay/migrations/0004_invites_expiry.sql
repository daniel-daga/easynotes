-- The sweep `create_invite` runs deletes by `expires_at`, which had no index.
--
-- Expand only (ADR 0029 §5): an index changes no table an older instance reads.
CREATE INDEX IF NOT EXISTS invites_expiry ON invites (expires_at);
