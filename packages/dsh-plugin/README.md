# @farwork/dsh-bridge

Farwork's own DSH bridge: a DSH **host plugin** that publishes a loopback JSON-RPC
endpoint, so `crates/farwork-agent --runtime dsh` can drive DSH sessions from
another device through the Farwork hub.

It is a from-scratch implementation of the bridge contract, not a fork or a copy of
`@agents-anywhere/dsh-bridge-next`. The two can run side by side because this one
rendezvouses in `~/.farwork/dsh-bridge/` (see "Coexistence").

```
DSH Host process
└── @farwork/dsh-bridge (Cordis plugin, this package)
    ├── manager lock   ~/.farwork/dsh-bridge/.manager-lock.json
    ├── endpoint.json  ~/.farwork/dsh-bridge/endpoint.json   (0600, dir 0700)
    └── TCP 127.0.0.1:<random>   JSON-RPC 2.0, one document per line
                     ▲
                     └── farwork-agent --runtime dsh (loopback client)
```

## What it serves (v1)

| Method | Meaning |
|---|---|
| `initialize` | token authentication, feature negotiation |
| `ping` | liveness |
| `session.list` | visible sessions, both identities per session |
| `session.getState` | live status + attachment state |
| `session.getSnapshot` | projected history, paged |
| `session.createAndStart` | create one DSH session and queue the first message |
| `session.startTurn` | queue a message in an existing session |
| `session.interrupt` | `cancel({kind:"user"})` on the live agent |
| `session.getCapabilities` | per-session capability set |
| `runtime.getCapabilities` | runtime-wide capability set |
| `runtime.sync.subscribe` | start one ordered event stream (snapshot baseline first) |
| `runtime.sync.ack` | acknowledge a batch |

Everything else — `session.getNotices`, `session.respondInteraction`,
`session.updateSelections`, `session.listCommands`, `session.executeCommand`,
`catalog.*`, `workspace.list`, `runtime.sync.progress/unsubscribe/refresh` —
answers `-32601 METHOD_NOT_FOUND`. A method that exists but is only half
implemented is worse than an honest refusal: the client would believe the answer.

### Why there is no approval path

DSH approvals and user questions are not reachable through a plain
`ctx.get("…")`. Upstream reaches them by reflecting into
`typertGateway.wireStream.open(...)`, branching on the function's `length` (5 vs 3
parameters), using `connection.browserPeer ?? connection.operator` as the peer, and
posting answers through a hand-built `Request` to `$events/result`. There is no
public precedent for that shape and no fail-soft version of it: if the internal
signature moves, the plugin either throws inside the host or, worse, silently
swallows the user's answer.

So v1 ships without it, and says so:

* `initialize` reports `features.approval = false` and `features.userQuestions = false`;
* `session.getNotices` / `session.respondInteraction` answer `-32601`.

**Degradation:** the browser shows no approval card, and `crates/farwork-agent`
does not forward one (its own `projection::notice` gates on those two flags). A
session that blocks on an approval must be answered **in the DSH web UI**, which
keeps working precisely because this plugin never touches the approval path. The
turn then continues and the timeline keeps flowing.

## Identity contract

Two identities per session, derived deterministically so a retried request lands on
the same session:

```
nativeId   = "aa_" + sha256(namespace)[0:16] + "_" + platformId   // externalSessionId
platformId = hub session id                                        // sessionId on the wire
```

* `sessionId` (platform) is what `crates/farwork-agent` sends on every call and what
  every sync operation carries.
* `externalSessionId` is DSH's own session id.

`sessionId` is resolved by **derivation**, not by enumerating sessions, so a
per-session call never depends on `sessionQuery` being loaded.

## Install (needs a DSH restart — do not do this while a DSH session is live)

```bash
# 0. build the package (dist/ is not committed)
node packages/dsh-plugin/scripts/build.mjs

# 1. link it into the desktop profile (dsh plugin forwards to pnpm)
dsh plugin --profile desktop add "link:/Users/tianjun/Desktop/prog/farwork/packages/dsh-plugin"

# 2. register the bundle: add "@farwork/dsh-bridge" to
#    ~/.dsh/profiles/desktop/package.json  ->  dsh.profile.bundles
#    (dsh plugin add installs the package; only the Desktop plugin manager or a
#     manual edit writes the bundles list)

# 3. quit DSH completely and start it again

# 4. verify the rendezvous file and who is listening
cat ~/.farwork/dsh-bridge/endpoint.json
lsof -nP -iTCP -sTCP:LISTEN | grep "$(node -p "require(process.env.HOME + '/.farwork/dsh-bridge/endpoint.json').port")"
```

Then point the device-side agent at it (the crate's default discovery path is still
the upstream directory, and this change is deliberately out of scope here):

```bash
cargo run -p farwork-agent -- \
  --token '<connectorToken>' --runtime dsh --workdir /srv/work \
  --dsh-endpoint ~/.farwork/dsh-bridge/endpoint.json
```

## Uninstall

```bash
# 1. drop the package from the profile
dsh plugin --profile desktop remove @farwork/dsh-bridge

# 2. remove "@farwork/dsh-bridge" from ~/.dsh/profiles/desktop/package.json -> dsh.profile.bundles

# 3. remove the rendezvous directory (safe once DSH is not running)
rm -rf ~/.farwork/dsh-bridge

# 4. restart DSH
```

The plugin also cleans up after itself on normal shutdown: it unpublishes
`endpoint.json` only if the file still carries its own token **and** pid, and
removes the lock only if it is still the owner.

## Coexistence with the upstream plugin

| | upstream | this plugin |
|---|---|---|
| endpoint | `~/.agents-anywhere/dsh-bridge/endpoint.json` | `~/.farwork/dsh-bridge/endpoint.json` |
| manager lock | `…/.agents-anywhere/dsh-bridge/.manager-lock.json` | `…/.farwork/dsh-bridge/.manager-lock.json` |
| TCP port | `listen(0)`, random | `listen(0)`, random |

The lock lives next to the endpoint file, so two different directories mean two
independent locks and no eviction. Never point this plugin at
`~/.agents-anywhere`: sharing that directory makes the two bridges overwrite each
other's endpoint and restart each other.

## Configuration

`cordis.patch.yml` inserts this plugin with an empty config. In the profile's patch
you can set:

```yaml
- id: farwork-dsh-bridge
  config:
    endpointDir: /Users/me/.farwork/dsh-bridge   # optional; must be absolute
```

`Config` is a standard-schema validator (Cordis 4 calls
`Config['~standard'].validate`), not a Schemastery schema: a plain defaults object
would throw inside the loader before `apply()` ever runs.

## Design notes / known limits

* **`apply()` never throws.** Every startup failure is `logger.warn` + return. A
  bridge that cannot start must cost the user the bridge, never the harness.
* **No imports of DSH packages.** A `link:`-installed third-party package cannot
  resolve `@deepseek-ai/cordis` (it only exists inside `app.asar`), so the host
  surface is probed duck-typed in `src/runtime.ts`. A missing service becomes a
  refusal at the method that needed it, not a load-time crash.
* **Projection is a deliberate subset.** Only assistant text, tool calls, tool
  results and interruption are projected. Reasoning blocks, images, compaction
  markers and approval records are dropped: `crates/farwork-agent` renders none of
  them, and claiming a full projection v3 would make a future checkpoint scheme
  replay less than it promises.
* **No checkpoint / upload-progress negotiation.** v1 always sends a full snapshot on
  subscribe and never asks the client to resume from a checkpoint, so
  `runtime.sync.subscribe` echoes neither `checkpointVersion` nor
  `uploadProgressVersion`.
* **`session.list` titles are `null`.** There is no title service in the v1 surface;
  an invented title would be a lie the browser renders.
* **Sessions created after `runtime.sync.subscribe` appear on the next subscribe.**
  v1 does not splice a half-built baseline into a live stream.
* **`features.syncMode` is `"polling"` when the host has no event bus.** Polling is
  not implemented either: it is the honest way to say no stream will be pushed, and
  `crates/farwork-agent` then refuses up front instead of going stale.

## Development

```bash
node scripts/build.mjs                 # strip types into dist/ (no bundler, no deps)
node --test dist/__tests__/*.test.js   # unit + socket tests
node scripts/selftest-bridge.mjs       # fake ctx, real TCP, temp endpoint dir
```

`scripts/build.mjs` uses Node's own `stripTypeScriptTypes`, so the package has **no
build-tool dependency at all** — important because DSH loads it with a bare
`import()` from a `link:` install. `tsconfig.json` exists for editors and for an
optional `tsc --noEmit` (needs `typescript` and `@types/node`; neither is a
dependency of this package).

No test and no self test touches `~/.dsh`, `~/.farwork`, port 62082 or a running
DSH: the endpoint directory is always a fresh `mkdtemp`.
