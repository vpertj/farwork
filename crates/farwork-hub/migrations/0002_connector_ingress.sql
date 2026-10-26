-- M3 connector ingress.
--
-- Two things the hub cannot keep in process memory:
--
-- * session_streams: the hub mints streamId and the connector echoes it on every
--   session-scoped frame with no other session identifier, so the mapping has to
--   survive a hub restart or item events from a still-running turn would be
--   unroutable. closed_at marks the end of a stream; an open row is what
--   session.message / session.interrupt address.
-- * session_notices: approvals are human-paced and outlive a reconnect: a pending
--   approval must still be pending after the browser reloads.

create table session_streams (
    stream_id text primary key,
    session_id text not null references sessions (id) on delete cascade,
    connector_id text not null references connectors (id) on delete cascade,
    created_at text not null,
    closed_at text
);

create index idx_session_streams_session on session_streams (session_id, closed_at);

create table session_notices (
    session_id text not null references sessions (id) on delete cascade,
    id text not null,
    kind text not null,
    status text not null,
    title text not null,
    body_json text not null default 'null',
    options_json text not null default '[]',
    decision text,
    created_at text not null,
    updated_at text not null,
    primary key (session_id, id)
);

-- notice.resolved carries only the notice id, so resolution looks the row up by id
-- and then checks it belongs to the connector that sent the frame.
create index idx_session_notices_id on session_notices (id);
