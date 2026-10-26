-- Device terminals (Ingress Protocol v1.2).
--
-- A terminal cannot live only in process memory: the browser reaches its stream
-- through a one-shot ws-ticket, and `closed_at` is what distinguishes "this
-- terminal is over" from "this terminal never existed" (terminal_not_found).
-- It is also the only durable record that the shell is gone, since neither side
-- of the relay holds a socket the hub can inspect after a restart.
--
-- cwd is the path the browser asked for, not a hub-side validated one: the
-- sandbox root is the agent's --workdir, which the hub never learns. The agent
-- resolves and refuses it (permission_denied on the stream).

create table terminals (
    id text primary key,
    connector_id text not null references connectors (id) on delete cascade,
    user_id text not null references users (id) on delete cascade,
    cols integer not null,
    rows integer not null,
    cwd text,
    created_at text not null,
    closed_at text,
    -- Why the agent refused (permission_denied, spawn_failed, ...). The refusal
    -- usually lands before the browser has finished minting its ticket, so the
    -- reason has to outlive the moment it was reported: without it a late attach
    -- could only say "already closed" and the sandbox escape would look like a
    -- terminal that simply vanished.
    error_code text,
    error_message text
);

-- "every open terminal of this connector", for the disconnect sweep.
create index idx_terminals_connector on terminals (connector_id, closed_at);

-- "is this terminal id mine", for the ws-ticket scope check.
create index idx_terminals_user on terminals (user_id, closed_at);