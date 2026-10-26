-- Session/message sharing.
--
-- A share is an immutable *copy*: snapshot_json is written once, under the
-- session's write lock, and never re-derived from the live timeline afterwards.
-- That is the whole point of the feature -- the original session keeps moving
-- (new messages, runtime updates) while the published link stays exactly as it
-- was when it was created. A reader that joined back to sessions/timeline_items
-- would silently turn a share into a live view, so the unauthenticated endpoint
-- reads nothing but this row.
--
-- deleted_at instead of a hard delete: the row is the only evidence that the id
-- was once valid, and a revoked link must answer the same 404 as one that never
-- existed (see routes::shares::delete_share).

create table shares (
    id text primary key,
    user_id text not null references users (id) on delete cascade,
    session_id text not null references sessions (id) on delete cascade,
    -- 'session' (whole timeline) or 'message' (the one item in message_id).
    scope text not null,
    message_id text,
    -- {"version":1,"sessionTitle":...,"eventCursor":"seq:N","items":[...]};
    -- items carry only id/type/status/role/content/orderSeq/itemTime.
    snapshot_json text not null,
    -- Attachment ids the snapshot is allowed to reference. Empty in this build:
    -- no share attachment route exists yet, and the whitelist is stored with the
    -- share so that route can never serve a file the owner did not publish.
    allowed_file_ids_json text not null default '[]',
    created_at text not null,
    deleted_at text
);

-- "my shares of this session", for the owner-facing list that will follow.
create index idx_shares_user_session on shares (user_id, session_id, created_at desc);
