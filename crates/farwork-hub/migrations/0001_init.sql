-- Farwork hub P0 schema.
-- Column names track upstream server/agent_server/infra/db/schema.py so the
-- repositories read the same way; the wire contract is what must match exactly.

create table users (
    id text primary key,
    email text,
    email_verified_at text,
    display_name text not null default '',
    -- pbkdf2_sha256$<salt>$<verifier>; the verifier is derived client-side so
    -- the server never stores the password.
    password_hash text not null,
    role text not null default 'member',
    disabled boolean not null default false,
    avatar text,
    created_at text not null,
    updated_at text not null
);

create unique index uq_users_email on users (lower(email)) where email is not null;

create table app_settings (
    key text primary key,
    value text not null,
    updated_at text not null
);

create table connectors (
    id text primary key,
    user_id text not null references users (id) on delete cascade,
    name text not null,
    connector_kind text not null default 'cli',
    device_os text,
    status text not null default 'offline',
    last_seen_at text,
    token_hash text not null,
    token_prefix text not null,
    revoked boolean not null default false,
    created_at text not null,
    updated_at text not null
);

create index idx_connectors_user on connectors (user_id);

create table projects (
    id text primary key,
    user_id text not null references users (id) on delete cascade,
    connector_id text not null references connectors (id) on delete cascade,
    name text not null,
    workspace_path text not null,
    workspace_key text not null,
    manually_created boolean not null default true,
    pinned boolean not null default false,
    pinned_at text,
    created_at text not null,
    updated_at text not null,
    constraint uq_projects_user_connector_workspace unique (user_id, connector_id, workspace_key),
    constraint uq_projects_user_name unique (user_id, name)
);

create index idx_projects_user on projects (user_id);

create table sessions (
    id text primary key,
    user_id text not null references users (id) on delete cascade,
    connector_id text not null references connectors (id) on delete cascade,
    project_id text references projects (id) on delete restrict,
    runtime text not null,
    runtime_id text not null,
    origin text not null default 'platform',
    external_session_id text,
    title text,
    title_source text,
    cwd text,
    status text not null default 'idle',
    takeover boolean not null default false,
    pinned boolean not null default false,
    pinned_at text,
    archived boolean not null default false,
    archived_at text,
    source_state text not null default 'visible',
    last_read_seq bigint not null default 0,
    latest_turn_end_seq bigint not null default 0,
    last_synced_at text,
    source_observed_at text,
    last_activity_at text,
    sort_at text not null,
    seq bigint not null default 0,
    seq_allocated_high bigint not null default 0,
    updated_seq bigint not null default 0,
    created_at text not null,
    updated_at text not null
);

create index idx_sessions_user_list on sessions (user_id, archived, pinned, sort_at desc, updated_seq desc);
create index idx_sessions_project on sessions (project_id);
create index idx_sessions_connector on sessions (connector_id);

create table session_runtime_states (
    session_id text primary key references sessions (id) on delete cascade,
    runtime text not null,
    external_session_id text,
    status text not null default 'idle',
    selections_json text not null default '{}',
    status_reason text,
    error_json text,
    metadata_json text not null default '{}',
    updated_seq bigint not null default 0,
    created_at text not null,
    updated_at text not null
);

create table timeline_items (
    session_id text not null references sessions (id) on delete cascade,
    id text not null,
    type text not null,
    status text not null,
    role text,
    order_seq integer not null,
    updated_seq bigint not null,
    revision integer not null default 1,
    content_hash text not null,
    client_message_id text,
    item_time text,
    payload_json text not null,
    primary key (session_id, id)
);

create index idx_timeline_items_session_updated_seq on timeline_items (session_id, updated_seq);
create index idx_timeline_items_session_order_seq on timeline_items (session_id, order_seq);
-- Idempotency for POST /sessions/{id}/runtime/messages (`clientMessageId`).
create unique index uq_timeline_items_client_message on timeline_items (session_id, client_message_id)
    where client_message_id is not null;

create table ws_tickets (
    ticket_hash text primary key,
    user_id text not null,
    client_id text not null,
    scope text not null,
    expires_at_epoch bigint not null,
    created_at text not null
);

create index idx_ws_tickets_expires on ws_tickets (expires_at_epoch);
