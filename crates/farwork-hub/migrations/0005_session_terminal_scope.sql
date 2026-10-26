-- Session-scoped terminals (Ingress Protocol v1.3).
--
-- A terminal opened from /sessions/{id}/terminal belongs to that session, not just
-- to its device, and the stream route refuses a terminal of the same device that
-- was opened somewhere else. Nullable on purpose: the device-namespace entries
-- created by 0003 have no session, and inventing one would misreport who opened
-- them. No second table: the PTY, its geometry, its close reason and its owner do
-- not differ between the two namespaces.

alter table terminals
    add column session_id text references sessions (id) on delete cascade;

-- "the terminals this session opened", for the session-namespace readers.
create index idx_terminals_session on terminals (session_id, closed_at);
