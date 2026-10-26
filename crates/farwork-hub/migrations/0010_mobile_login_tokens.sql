-- One-time QR login tokens for the mobile client.
--
-- Only token_hash is stored, never the token: the QR code is rendered from the
-- plaintext and photographed by a phone, so the plaintext is a credential in the
-- same sense a ws_ticket is. A database dump must not be replayable into a login.
--
-- Consumption is a delete, not a flag (`delete ... returning`, see
-- routes::mobile_login): a consumed token and a never-issued one are
-- indistinguishable afterwards, which is exactly the answer an unauthenticated
-- exchange endpoint should be able to give.
--
-- expires_at_epoch instead of an ISO column: expiry is a comparison, and an
-- integer cannot be read differently by two code paths (same shape as
-- ws_tickets.expires_at_epoch). created_at stays ISO text because it only ever
-- leaves the server as a wire timestamp.

create table mobile_login_tokens (
    token_hash text primary key,
    user_id text not null references users (id) on delete cascade,
    expires_at_epoch bigint not null,
    created_at text not null
);

-- Matches the periodic-sweep shape ws_tickets already uses: drop the rows whose
-- window has closed instead of scanning the whole table.
create index idx_mobile_login_tokens_expires on mobile_login_tokens (expires_at_epoch);
