-- notice.upsert gained an optional `interactionType` (`approval`, `question`, ...).
--
-- A DSH approval is a generic runtime interaction, not one of the three `kind`
-- values the notice column has always carried, so the interaction type gets its
-- own nullable column instead of overloading `kind`: reusing `kind` would rename
-- what the browser already renders for codex approvals.
alter table session_notices add column if not exists interaction_type text;
