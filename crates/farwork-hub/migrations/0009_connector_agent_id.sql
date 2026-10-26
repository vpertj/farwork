-- Device identity: which physical machine a connector row is.
--
-- agent_id is the stable id the agent keeps in $FARWORK_HOME/agent.json (it
-- survives restarts and renames, and deleting the file makes a new one). Pairing
-- used to create a row per claim, so one laptop that paired twice showed up
-- twice; with this column the hub can recognise "this machine again".
--
-- Deliberately *not* keyed on name or hostname: two devices may share a name and
-- a device may be renamed in the UI, so a name-keyed dedupe would merge two
-- different machines or split one.
--
-- Partial unique index because null means "never reported an identity": rows that
-- predate this migration and agents too old to send one keep working, and any
-- number of them may coexist under one account. Only real identities are unique.

alter table connectors add column agent_id text;

-- The claim lookup is (user, device): the same machine on two accounts is two
-- devices, and the same identity twice under one account is the duplicate this
-- index exists to prevent -- including from two concurrent claims.
create unique index uq_connectors_user_agent_id
    on connectors (user_id, agent_id)
    where agent_id is not null;
