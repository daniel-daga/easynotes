-- ADR 0027 §5: a committed object no compaction lists any more is doomed, and
-- deleted a week later unless something asks for it first. NULL is live.
-- Unix seconds, like `touched_at`.
--
-- Expand only (ADR 0029 §5): a relay from before this migration runs on the
-- table as it is after it, and simply never dooms anything.
ALTER TABLE media_objects ADD COLUMN IF NOT EXISTS doomed_at BIGINT;

CREATE INDEX IF NOT EXISTS media_objects_doomed
    ON media_objects (doomed_at) WHERE doomed_at IS NOT NULL;
