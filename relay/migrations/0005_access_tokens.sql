-- ADR 0038: one random token per device. There is no household column; the
-- token is the device's whole credential, so `secret` is BYTEA and never text.
-- `device_key` is NULL until the token is first used, and the bind is one
-- conditional UPDATE, so two devices cannot both claim it.
CREATE TABLE access_tokens (
    id          TEXT PRIMARY KEY,
    secret      BYTEA NOT NULL,
    device_key  BYTEA,
    read_only   BOOLEAN NOT NULL DEFAULT FALSE,
    expires_at  BIGINT,
    revoked_at  BIGINT
);

-- The 30-day revoked-token purge will read by `revoked_at`; nothing else does.
CREATE INDEX access_tokens_revoked ON access_tokens (revoked_at)
    WHERE revoked_at IS NOT NULL;
