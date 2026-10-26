-- Pairing codes: the one-time secret a device trades for a connector token.
--
-- Only code_hash is stored, never the code itself. The digest is keyed by the
-- hub secret (see `pairing::hash_code`): an 8-character code carries ~39 bits,
-- so an unkeyed digest in a database dump could be reversed offline in minutes
-- and replayed against a still-pending pairing.
--
-- expires_at_epoch instead of an ISO column: expiry is a comparison, and an
-- integer cannot be read differently by two code paths (same shape as
-- ws_tickets.expires_at_epoch). created_at/claimed_at stay ISO text because they
-- only ever leave the server as wire timestamps.
--
-- claimed_at is the one-time switch: claim updates it with
-- `where claimed_at is null`, so two concurrent claims of the same code cannot
-- both create a connector. connector_id points at the device the claim created;
-- it is nullable so deleting that connector does not erase the evidence that the
-- code was used.

create table pairings (
    id text primary key,
    user_id text not null references users (id) on delete cascade,
    code_hash text not null,
    created_at text not null,
    expires_at_epoch bigint not null,
    claimed_at text,
    connector_id text references connectors (id) on delete set null,
    -- Source address of the successful claim (the rate-limit key), for auditing
    -- a pairing that a user says they did not initiate.
    attempt_ip text
);

-- The claim lookup is by digest alone: the unauthenticated caller cannot say
-- which user the code belongs to. Unique as well, so a collision can be retried
-- instead of silently shadowing a live code.
create unique index uq_pairings_code_hash on pairings (code_hash);

-- "my pairing codes", newest first, for the browser's status list.
create index idx_pairings_user on pairings (user_id, created_at desc);
