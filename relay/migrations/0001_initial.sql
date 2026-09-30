-- Migration 1: the schema every relay had before there were migrations.
--
-- Written so it changes nothing on a database that already has it: a relay
-- that ran the old start-up code adopts this as applied without a hand step
-- (ADR 0029 §5). That is why everything here says IF NOT EXISTS, and why the
-- columns that arrived later are added rather than declared. From migration 2
-- on, a file assumes the one before it ran, and says so plainly instead.
--
-- Once a release has shipped a file here, it is never edited again: a change
-- is a new file with the next number.

-- Two tables, because the sequence number has to be allocated atomically.
--
-- `note_head` holds one row per note carrying its highest `seq`; an
-- `ON CONFLICT DO UPDATE ... RETURNING` against it takes a row lock and hands
-- back the next number in a single statement, so concurrent appenders queue
-- rather than collide. `note_log` holds the envelopes.
--
-- The obvious `SELECT MAX(seq) + 1` does not work here: readers do not block
-- each other, so sixteen concurrent appends all compute the same number and
-- fifteen of them lose on the primary key.
--
-- `seq` is per note rather than global so that a client's `since` cursor is
-- dense and means what it looks like it means.
CREATE TABLE IF NOT EXISTS note_log (
    note_id     TEXT   NOT NULL,
    seq         BIGINT NOT NULL,
    envelope    BYTEA  NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (note_id, seq)
);

CREATE TABLE IF NOT EXISTS note_head (
    note_id TEXT   PRIMARY KEY,
    seq     BIGINT NOT NULL
);

-- ADR 0024: what compaction must leave, and each topic's bookkeeping.
ALTER TABLE note_log ADD COLUMN IF NOT EXISTS keep BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE note_head ADD COLUMN IF NOT EXISTS live_envelopes BIGINT NOT NULL DEFAULT 0;
ALTER TABLE note_head ADD COLUMN IF NOT EXISTS live_bytes BIGINT NOT NULL DEFAULT 0;
ALTER TABLE note_head ADD COLUMN IF NOT EXISTS loose_envelopes BIGINT NOT NULL DEFAULT 0;
ALTER TABLE note_head ADD COLUMN IF NOT EXISTS loose_bytes BIGINT NOT NULL DEFAULT 0;
ALTER TABLE note_head ADD COLUMN IF NOT EXISTS owner BYTEA;
ALTER TABLE note_head ADD COLUMN IF NOT EXISTS current_revision BIGINT;
ALTER TABLE note_head ADD COLUMN IF NOT EXISTS current_digest BYTEA;
ALTER TABLE note_head ADD COLUMN IF NOT EXISTS current_compaction BYTEA;
ALTER TABLE note_head ADD COLUMN IF NOT EXISTS registered BYTEA;
ALTER TABLE note_head ADD COLUMN IF NOT EXISTS covers BIGINT NOT NULL DEFAULT 0;
-- ADR 0026: a purged topic's tombstone.
ALTER TABLE note_head ADD COLUMN IF NOT EXISTS gone BOOLEAN NOT NULL DEFAULT FALSE;

-- Topics stored before there was any bookkeeping start with none. Counted
-- once: a topic with anything in it never counts zero again, because a
-- compaction leaves at least its base.
UPDATE note_head h SET
    live_envelopes = c.n, live_bytes = c.b,
    loose_envelopes = c.n, loose_bytes = c.b
FROM (
    SELECT l.note_id, count(*) AS n, COALESCE(sum(length(l.envelope)), 0) AS b
    FROM note_log l JOIN note_head z ON z.note_id = l.note_id
    WHERE z.live_envelopes = 0 AND z.seq > 0
    GROUP BY l.note_id
) c
WHERE h.note_id = c.note_id;

CREATE TABLE IF NOT EXISTS auth_challenges (
    nonce      BYTEA PRIMARY KEY,
    device     BYTEA NOT NULL,
    expires_at BIGINT NOT NULL
);

-- ADR 0027: attachment objects. The bytes are in the media directory; this is
-- what is reserved and what is committed.
CREATE TABLE IF NOT EXISTS media_objects (
    note_id    TEXT    NOT NULL,
    object_id  TEXT    NOT NULL,
    sealed_len BIGINT  NOT NULL,
    chunk_size INTEGER NOT NULL,
    digest     BYTEA,
    touched_at BIGINT  NOT NULL,
    PRIMARY KEY (note_id, object_id)
);

CREATE INDEX IF NOT EXISTS media_objects_staging
    ON media_objects (touched_at) WHERE digest IS NULL;

-- Invites. `expires_at` is Unix seconds, the same number the memory store
-- keeps, so the two cannot round differently. `grant_blob` because `grant` is
-- a keyword.
CREATE TABLE IF NOT EXISTS invites (
    invite_id  TEXT    PRIMARY KEY,
    token_hash BYTEA   NOT NULL,
    owner_hash BYTEA   NOT NULL,
    expires_at BIGINT  NOT NULL,
    open       BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE TABLE IF NOT EXISTS invite_requests (
    invite_id  TEXT     NOT NULL REFERENCES invites (invite_id) ON DELETE CASCADE,
    request_id TEXT     NOT NULL,
    payload    BYTEA    NOT NULL,
    state      SMALLINT NOT NULL DEFAULT 0,
    grant_blob BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (invite_id, request_id)
);
