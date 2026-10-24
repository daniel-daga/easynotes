-- ADR 0030: the note key sealed for whoever claims a link, so a guest can read
-- the note without waiting for the owner's device. NULL on every invite made
-- before this, and on one made by a client that does not seal one.
--
-- Expand only (ADR 0029 §5): an older relay instance keeps working against this
-- table, and an older client simply never sends or reads it.
ALTER TABLE invites ADD COLUMN IF NOT EXISTS pre_grant BYTEA;
