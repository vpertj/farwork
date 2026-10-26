import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync } from 'node:fs'
import { mkdir, readFile, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { test } from 'node:test'

import { Bridge } from '../bridge.js'
import type { BridgeStatus } from '../bridge.js'
import { NO_LOGGER } from '../logger.js'
import type { Logger } from '../logger.js'
import { installPanel, PANEL_CHANNEL } from '../panel.js'
import type { PanelBridge, PanelHandler, PanelOk, PanelRegistration, PanelReply } from '../panel.js'

const PACKAGE_VERSION = (JSON.parse(
  readFileSync(new URL('../../package.json', import.meta.url), 'utf8'),
) as { version: string }).version

const ENDPOINT = '/tmp/farwork-panel-test/endpoint.json'

/**
 * A fresh endpoint directory per installation.
 *
 * panel.json outlives the process on purpose, so two installations sharing one
 * directory would leak the first test's hub into the next one's startup read.
 */
function freshEndpoint(): string {
  return join(mkdtempSync(join(tmpdir(), 'farwork-panel-')), 'endpoint.json')
}
const STATUS: BridgeStatus = { ready: true, port: 7501, endpointPath: ENDPOINT, pid: 4242, lastError: null }

function fakeBridge(overrides: Partial<BridgeStatus> = {}): PanelBridge {
  return {
    get status(): BridgeStatus {
      return { ...STATUS, ...overrides }
    },
    endpointFile: ENDPOINT,
  }
}

interface Registration {
  channel: string
  handler: PanelHandler
  released: number
  /** The identity of the context the route was filed under. */
  owner: string
}

interface FakeRoute {
  channel: string
  handler: PanelHandler
  released: number
}

/**
 * A context of the shape Cordis really presents: reading a service the context did
 * not declare throws "cannot get property X without inject" instead of answering
 * undefined. The previous fake answered every name from one dictionary, which is
 * how a registration that reached webServer through the *wrong* context passed this
 * suite while failing on a real host.
 */
interface FiberSpec {
  identity: string
  /** Service names this context declared, i.e. its Cordis `inject` list. */
  declared: string[]
  services?: Record<string, unknown>
  /** Every read this proxy served, undeclared names prefixed with '!'. */
  reads?: string[]
  inject?: (services: string[], callback: (ctx: unknown) => void) => unknown
}

function fiberCtx(spec: FiberSpec): unknown {
  const declared = new Set(spec.declared)
  const services = spec.services ?? {}
  const reads = spec.reads ?? []
  const own: Record<string, unknown> = {
    farworkIdentity: spec.identity,
    get(name: string): unknown {
      reads.push('get:' + name)
      return declared.has(name) ? services[name] : undefined
    },
    // Cordis runs an effect body immediately and disposes it with the fiber.
    effect(callback: () => unknown): () => void {
      const disposer = callback()
      return typeof disposer === 'function' ? (disposer as () => void) : () => undefined
    },
    inject: spec.inject,
  }
  return new Proxy(own, {
    get(inner, prop, receiver): unknown {
      if (typeof prop !== 'string' || prop in inner) return Reflect.get(inner, prop, receiver)
      if (declared.has(prop)) {
        reads.push(prop)
        return services[prop]
      }
      reads.push('!' + prop)
      throw new Error('cannot get property "' + prop + '" without inject')
    },
  })
}

function identityOf(ctx: unknown): string {
  const mark = (ctx as { farworkIdentity?: unknown } | null | undefined)?.farworkIdentity
  return typeof mark === 'string' ? mark : 'unknown'
}

/** The webServer service: a route registry that hands back a disposer per route. */
function webServerService(routes: FakeRoute[]): unknown {
  return {
    register(channel: string, handler: PanelHandler): () => void {
      const route: FakeRoute = { channel, handler, released: 0 }
      routes.push(route)
      return () => {
        route.released += 1
      }
    },
  }
}

/**
 * The connection service as DSH ships it: a channel is mounted by registering an
 * HTTP route on the *owner* context's webServer, so an owner that never declared
 * webServer throws inside the registration. `rpc.handle` is included because the
 * shipped service has it - and it routes through the context the service itself was
 * created in, which is a context that never declared webServer.
 */
function connectionService(
  registrations: Registration[],
  options: { withRpc?: boolean; onRpc?: () => void } = {},
): Record<string, unknown> {
  const service: Record<string, unknown> = {
    register(owner: unknown, channel: string, handler: PanelHandler): () => void {
      const webServer = (owner as { webServer?: { register: (c: string, h: PanelHandler) => () => void } }).webServer
      if (webServer === undefined) throw new Error('cannot get property "webServer" without inject')
      const release = webServer.register(channel, handler)
      const entry: Registration = { channel, handler, released: 0, owner: identityOf(owner) }
      registrations.push(entry)
      return () => {
        entry.released += 1
        release()
      }
    },
  }
  if (options.withRpc !== false) {
    service.rpc = {
      handle(): never {
        options.onRpc?.()
        throw new Error('cannot get property "webServer" without inject')
      },
    }
  }
  return service
}

/** A registry that needs no webServer, so any webServer read below belongs to us. */
function recordingConnectionService(registrations: Registration[]): unknown {
  return {
    register(owner: unknown, channel: string, handler: PanelHandler): () => void {
      const entry: Registration = { channel, handler, released: 0, owner: identityOf(owner) }
      registrations.push(entry)
      return () => {
        entry.released += 1
      }
    },
  }
}

function fakeConnection(): { ctx: unknown; registrations: Registration[]; routes: FakeRoute[] } {
  const registrations: Registration[] = []
  const routes: FakeRoute[] = []
  return {
    ctx: {
      farworkIdentity: 'plugin',
      effect: (callback: () => unknown): (() => void) => {
        const disposer = callback()
        return typeof disposer === 'function' ? (disposer as () => void) : () => undefined
      },
      webServer: webServerService(routes),
      get: (name: string) => (name === 'connection' ? connectionService(registrations) : undefined),
    },
    registrations,
    routes,
  }
}

interface LogLine {
  level: string
  message: string
  details?: unknown
}

function captureLogger(): { logger: Logger; lines: LogLine[] } {
  const lines: LogLine[] = []
  const at = (level: string) => (message: string, details?: unknown): void => {
    lines.push({ level, message, details })
  }
  return {
    logger: { debug: at('debug'), info: at('info'), warn: at('warn'), error: at('error') },
    lines,
  }
}

/** A host whose connection service is only reachable through ctx.inject. */
function injectHost(returned?: unknown): {
  ctx: unknown
  injects: Array<{ services: string[]; callback: (ctx: unknown) => void }>
} {
  const injects: Array<{ services: string[]; callback: (ctx: unknown) => void }> = []
  const ctx = fiberCtx({
    identity: 'plugin',
    declared: [],
    inject(services: string[], callback: (ctx: unknown) => void): unknown {
      injects.push({ services, callback })
      return returned
    },
  })
  return { ctx, injects }
}

function install(bridge: PanelBridge, endpointFile: string = freshEndpoint()) {
  const { ctx, registrations } = fakeConnection()
  const scoped: PanelBridge = {
    get status(): BridgeStatus {
      return bridge.status
    },
    endpointFile,
  }
  const registration = installPanel({ ctx, bridge: scoped, logger: NO_LOGGER() })
  assert.equal(registrations.length, 1)
  return { handler: registrations[0]!.handler, registration, registrations, endpointFile }
}

function valueOf(reply: PanelReply): Record<string, unknown> {
  assert.equal(reply.ok, true)
  return (reply as PanelOk).value
}

test('the panel registers exactly one channel and it is not /api', () => {
  const { registrations } = install(fakeBridge())
  assert.equal(PANEL_CHANNEL, '/farwork')
  assert.equal(registrations.length, 1)
  assert.equal(registrations[0]!.channel, '/farwork')
})

test('status reports the seven panel fields and leaks no endpoint token', async () => {
  const leaking = { ...STATUS, token: 'endpoint-token-must-not-leak' } as unknown as BridgeStatus
  const { handler } = install({ endpointFile: ENDPOINT, get status() { return leaking } })

  const value = valueOf(await handler('status', {}))
  assert.deepEqual(value, {
    ready: true,
    port: 7501,
    endpointPath: ENDPOINT,
    pid: 4242,
    lastError: null,
    version: PACKAGE_VERSION,
    hub: null,
  })
  assert.equal(JSON.stringify(value).includes('endpoint-token-must-not-leak'), false)
})

test('a bridge that failed to start still explains itself through status', async () => {
  const lastError = "EACCES: permission denied, mkdir '/root/.farwork/dsh-bridge'"
  const { handler } = install(fakeBridge({ ready: false, port: 0, lastError }))

  const value = valueOf(await handler('status', {}))
  assert.equal(value.ready, false)
  assert.equal(value.port, 0)
  assert.equal(value.lastError, lastError)
})

test('command quotes the endpoint path and keeps a placeholder until a token is known', async () => {
  const { handler, endpointFile } = install(fakeBridge())
  const value = valueOf(await handler('command', {}))
  assert.equal(
    value.command,
    "farwork-agent --token '<connectorToken>' --runtime dsh --dsh-endpoint '" + endpointFile + "'",
  )
})

test('a pasted token is used for one command and is not persisted by reading it', async () => {
  const { handler, endpointFile } = install(fakeBridge())
  const pasted = valueOf(await handler('command', { connectorToken: 'ct_pasted' }))
  assert.equal(
    pasted.command,
    "farwork-agent --token 'ct_pasted' --runtime dsh --dsh-endpoint '" + endpointFile + "'",
  )

  const again = valueOf(await handler('command', {}))
  assert.equal(
    again.command,
    "farwork-agent --token '<connectorToken>' --runtime dsh --dsh-endpoint '" + endpointFile + "'",
  )
})

test('savePair remembers the token so a later command uses it, without echoing it', async () => {
  const { handler, endpointFile } = install(fakeBridge())
  const saved = await handler('savePair', { connectorToken: 'ct_saved', hub: 'https://hub.example.test' })
  const value = valueOf(saved)
  assert.deepEqual(value, { accepted: true, hasToken: true, hub: 'https://hub.example.test' })
  assert.equal(JSON.stringify(value).includes('ct_saved'), false)

  const command = valueOf(await handler('command', {}))
  assert.equal(
    command.command,
    "farwork-agent --token 'ct_saved' --runtime dsh --dsh-endpoint '" + endpointFile + "'",
  )
})

test('savePair refuses a hub that is not a url and a token that is not a string', async () => {
  const { handler } = install(fakeBridge())
  const badHub = await handler('savePair', { hub: 'hub.example.test' })
  assert.equal(badHub.ok, false)
  assert.equal(badHub.ok === false ? badHub.error.code : '', 'farwork/invalid_hub')

  const badToken = await handler('savePair', { connectorToken: 42 })
  assert.equal(badToken.ok, false)
  assert.equal(badToken.ok === false ? badToken.error.code : '', 'farwork/invalid_token')
})

test('status echoes the saved hub but never the saved token', async () => {
  const { handler } = install(fakeBridge())
  assert.equal(valueOf(await handler('status', {})).hub, null)

  valueOf(await handler('savePair', { connectorToken: 'ct_keep_secret', hub: 'https://hub.example.test/' }))

  // A browser that reopens the panel reads both back from status: the hub is a
  // public URL, the token is a credential and stays host-side.
  const value = valueOf(await handler('status', {}))
  assert.equal(value.hub, 'https://hub.example.test/')
  assert.equal(JSON.stringify(value).includes('ct_keep_secret'), false)
  assert.equal(value.hasToken, undefined)
})

test('a refused hub keeps the previously saved hub', async () => {
  const { handler } = install(fakeBridge())
  valueOf(await handler('savePair', { hub: 'https://hub.example.test' }))

  const refused = await handler('savePair', { hub: 'ftp://hub.example.test' })
  assert.equal(refused.ok, false)
  assert.equal(refused.ok === false ? refused.error.code : '', 'farwork/invalid_hub')
  assert.match(refused.ok === false ? refused.error.message : '', /http\(s\):\/\/|ws\(s\):\/\//)
  assert.equal(valueOf(await handler('status', {})).hub, 'https://hub.example.test')
})

test('a pairing code is refused as a pairing code, not stored as a token', async () => {
  const { handler } = install(fakeBridge())

  for (const code of ['AB3D-EFGH', 'ab3d-efgh']) {
    const refused = await handler('savePair', { connectorToken: code })
    assert.equal(refused.ok, false)
    assert.equal(refused.ok === false ? refused.error.code : '', 'farwork/pairing_code')
    const message = refused.ok === false ? refused.error.message : ''
    // The reason has to name the actual fix: mint a device in the Web console.
    assert.match(message, /配对码/)
    assert.match(message, /Web 控制台/)
    assert.match(message, /connectorToken/)
  }

  // Nothing was stored: the command still shows the placeholder, which is the only
  // honest state after a refused save.
  const command = valueOf(await handler('command', {}))
  assert.match(command.command, /'<connectorToken>'/)
})

test('a token that is not a pairing code shape is still accepted', async () => {
  const { handler } = install(fakeBridge())
  const saved = await handler('savePair', { connectorToken: 'cxt_0123456789abcdef' })
  assert.equal(valueOf(saved).hasToken, true)
})

test('savePair writes the hub to panel.json beside the endpoint and a restart reads it back', async () => {
  const endpointFile = freshEndpoint()
  const first = install(fakeBridge(), endpointFile)
  valueOf(await first.handler('savePair', { hub: 'https://hub.example.test' }))

  const file = join(dirname(endpointFile), 'panel.json')
  assert.deepEqual(JSON.parse(await readFile(file, 'utf8')), { version: 1, hub: 'https://hub.example.test' })
  assert.equal((await stat(file)).mode & 0o777, 0o600, 'panel.json must be private')
  assert.equal((await stat(dirname(file))).mode & 0o777, 0o700, 'its directory must be private')
  // Nothing about the hub touches endpoint.json: that file is the agent's
  // discovery contract and keeps exactly the keys it documents.
  await assert.rejects(readFile(endpointFile, 'utf8'))

  // A second installation of the panel is what a DSH restart looks like from here.
  const restarted = install(fakeBridge(), endpointFile)
  assert.equal(valueOf(await restarted.handler('status', {})).hub, 'https://hub.example.test')
})

test('the panel file holds the hub and never the connector token', async () => {
  const endpointFile = freshEndpoint()
  const { handler } = install(fakeBridge(), endpointFile)
  valueOf(
    await handler('savePair', { connectorToken: 'ct_must_not_land_on_disk', hub: 'https://hub.example.test' }),
  )

  const raw = await readFile(join(dirname(endpointFile), 'panel.json'), 'utf8')
  assert.equal(raw.includes('ct_must_not_land_on_disk'), false, 'a credential must never be written: ' + raw)
  assert.deepEqual(JSON.parse(raw), { version: 1, hub: 'https://hub.example.test' })
  // The token still works for this process, it just is not persisted.
  assert.match(valueOf(await handler('command', {})).command, /ct_must_not_land_on_disk/)
})

test('a corrupt panel file reads as unconfigured and never stops the panel', async () => {
  const cases = [
    'not json at all',
    '{"version":1,"hub":42}',
    '{"version":2,"hub":"https://hub.example.test"}',
    '{"version":1,"hub":"   "}',
    '{"version":1,"hub":"ftp://hub.example.test"}',
    '[]',
  ]
  for (const contents of cases) {
    const endpointFile = freshEndpoint()
    await writeFile(join(dirname(endpointFile), 'panel.json'), contents, 'utf8')
    // Installing must not throw on any of them.
    const { handler } = install(fakeBridge(), endpointFile)
    assert.equal(valueOf(await handler('status', {})).hub, null, 'unexpected hub for: ' + contents)
    // A bad file is not a broken save: a good hub is still accepted afterwards.
    assert.equal(valueOf(await handler('savePair', { hub: 'https://hub.new.test' })).hub, 'https://hub.new.test')
  }
})

test('a hub that cannot be written is reported as a failure, not as saved', async () => {
  const endpointFile = freshEndpoint()
  const file = join(dirname(endpointFile), 'panel.json')
  // A directory where the record belongs makes the atomic rename fail, which is
  // what a read-only or full disk presents to the writer.
  await mkdir(file)
  const { handler } = install(fakeBridge(), endpointFile)

  const reply = await handler('savePair', { hub: 'https://hub.example.test' })
  assert.equal(reply.ok, false)
  assert.equal(reply.ok === false ? reply.error.code : '', 'farwork/hub_not_saved')
  const message = reply.ok === false ? reply.error.message : ''
  assert.match(message, /hub/)
  assert.match(message, /panel\.json/)
  // Not applied in memory either: nothing may look saved that is not on disk.
  assert.equal(valueOf(await handler('status', {})).hub, null)
})

test('an unknown endpoint is refused with a readable code instead of throwing', async () => {
  const { handler } = install(fakeBridge())
  const reply = await handler('nope', {})
  assert.equal(reply.ok, false)
  assert.equal(reply.ok === false ? reply.error.code : '', 'farwork/unknown_endpoint')
})

test('a failing endpoint answers ok:false and the channel keeps serving', async () => {
  let fail = true
  const bridge: PanelBridge = {
    get status(): BridgeStatus {
      if (fail) throw new Error('boom from the bridge')
      return STATUS
    },
    endpointFile: ENDPOINT,
  }
  const { handler } = install(bridge)

  const broken = await handler('status', {})
  assert.equal(broken.ok, false)
  assert.equal(broken.ok === false ? broken.error.code : '', 'farwork/panel_failed')
  assert.match(broken.ok === false ? broken.error.message : '', /boom from the bridge/)

  fail = false
  const recovered = valueOf(await handler('status', {}))
  assert.equal(recovered.ready, true)
})

test('the disposer releases the channel exactly once', () => {
  const { registration, registrations } = install(fakeBridge())
  registration.dispose()
  registration.dispose()
  assert.equal(registrations[0]!.released, 1)
})

test('no connection service leaves the panel inert instead of throwing', () => {
  for (const ctx of [undefined, null, {}, { get: () => undefined }, { get: () => ({}) }]) {
    assert.doesNotThrow(() => {
      const registration = installPanel({ ctx, bridge: fakeBridge(), logger: NO_LOGGER() })
      registration.dispose()
    })
  }
})

test('the channel is also found when the connection service is a plain property', async () => {
  const registrations: Registration[] = []
  const ctx = {
    connection: {
      // No `register`: this host exposes only the rpc.handle form, which is the
      // documented one, so the fallback has to keep working.
      rpc: {
        handle(channel: string, handler: PanelHandler): () => void {
          registrations.push({ channel, handler, released: 0, owner: 'rpc.handle' })
          return () => undefined
        },
      },
    },
  }
  const registration = installPanel({ ctx, bridge: fakeBridge(), logger: NO_LOGGER() })
  assert.equal(registrations.length, 1)
  const value = valueOf(await registrations[0]!.handler('status', {}))
  assert.equal(value.ready, true)
  registration.dispose()
})

test('a refused registration is reported and swallowed', () => {
  const ctx = {
    get: () => ({
      rpc: {
        handle() {
          throw new Error('already registered')
        },
      },
    }),
  }
  let registration: { dispose(): void } | null = null
  assert.doesNotThrow(() => {
    registration = installPanel({ ctx, bridge: fakeBridge(), logger: NO_LOGGER() })
  })
  assert.doesNotThrow(() => registration!.dispose())
})

test('a direct registration logs the channel and the mode', () => {
  const { logger, lines } = captureLogger()
  const { ctx, registrations } = fakeConnection()

  installPanel({ ctx, bridge: fakeBridge(), logger }).dispose()

  const infos = lines.filter((line) => line.level === 'info')
  assert.equal(infos.length, 1)
  assert.match(infos[0]!.message, /\/farwork/)
  assert.match(infos[0]!.message, /mode: direct/)
  assert.equal(registrations[0]!.released, 1)
})

test('a host without connection yet registers through ctx.inject once it appears', async () => {
  const { logger, lines } = captureLogger()
  const registrations: Registration[] = []
  const routes: FakeRoute[] = []
  const injects: Array<{ services: string[]; callback: (ctx: unknown) => void }> = []
  const injectReleases: string[] = []
  const ctx = {
    get: () => undefined,
    inject(services: string[], callback: (ctx: unknown) => void): () => void {
      injects.push({ services, callback })
      return () => {
        injectReleases.push('inject')
      }
    },
  }

  const registration = installPanel({ ctx, bridge: fakeBridge(), logger })

  // Nothing is registered yet, but the panel is no longer permanently inert: it
  // subscribed to the services instead.
  assert.equal(registrations.length, 0)
  // Both services, because the route is filed under the context that declared
  // them: a context holding only `connection` cannot reach webServer.
  assert.deepEqual(injects.map((entry) => entry.services), [['connection', 'webServer']])
  assert.match(lines.at(-1)!.message, /waiting for connection \+ webServer \(mode: inject\)/)

  injects[0]!.callback(
    fiberCtx({
      identity: 'inject-callback',
      declared: ['connection', 'webServer'],
      services: { connection: connectionService(registrations), webServer: webServerService(routes) },
    }),
  )

  assert.equal(registrations.length, 1)
  assert.equal(registrations[0]!.channel, PANEL_CHANNEL)
  assert.equal(registrations[0]!.owner, 'inject-callback')
  assert.equal(routes.length, 1)
  assert.equal(routes[0]!.channel, PANEL_CHANNEL)
  const infos = lines.filter((line) => line.level === 'info')
  assert.match(infos.at(-1)!.message, /registered \(mode: inject\)/)
  const value = valueOf(await registrations[0]!.handler('status', {}))
  assert.equal(value.ready, true)

  // Both the channel route and the injection itself are released once.
  registration.dispose()
  registration.dispose()
  assert.equal(registrations[0]!.released, 1)
  assert.equal(routes[0]!.released, 1)
  assert.deepEqual(injectReleases, ['inject'])
})

test('a refused direct registration falls through to ctx.inject instead of giving up', async () => {
  const { logger, lines } = captureLogger()
  const registrations: Registration[] = []
  const routes: FakeRoute[] = []
  const injects: Array<{ services: string[]; callback: (ctx: unknown) => void }> = []
  // The plugin context reaches the service, but it never declared webServer, so the
  // registration throws before any route exists and the browser would see HTTP 405
  // on /farwork/status. Falling through is what turns that into a working panel.
  const refusing = {
    register(_owner: unknown, _channel: string, _handler: PanelHandler): () => void {
      throw new Error('cannot get property "webServer" without inject')
    },
  }
  const ctx = {
    get: () => refusing,
    inject(services: string[], callback: (ctx: unknown) => void): () => void {
      injects.push({ services, callback })
      return () => undefined
    },
  }

  const registration = installPanel({ ctx, bridge: fakeBridge(), logger })

  assert.equal(registrations.length, 0)
  // Quiet, not a warning: a linked plugin on DSH always takes this path, so the
  // line has to stay available without alarming anyone reading the log.
  assert.equal(lines[0]!.level, 'debug')
  assert.match(lines[0]!.message, /could not be registered directly; trying ctx\.inject/)
  assert.deepEqual(injects[0]!.services, ['connection', 'webServer'])

  injects[0]!.callback(
    fiberCtx({
      identity: 'inject-callback',
      declared: ['connection', 'webServer'],
      services: { connection: connectionService(registrations), webServer: webServerService(routes) },
    }),
  )
  assert.equal(registrations.length, 1)
  assert.equal(registrations[0]!.channel, PANEL_CHANNEL)
  assert.equal(registrations[0]!.owner, 'inject-callback')
  assert.match(lines.filter((line) => line.level === 'info').at(-1)!.message, /registered \(mode: inject\)/)
  const value = valueOf(await registrations[0]!.handler('status', {}))
  assert.equal(value.ready, true)

  registration.dispose()
  assert.equal(registrations[0]!.released, 1)
})

test('a deferred callback that carries no rpc.handle stays inert instead of throwing', () => {
  const { logger, lines } = captureLogger()
  const injects: Array<(ctx: unknown) => void> = []
  const ctx = {
    get: () => undefined,
    inject(_services: string[], callback: (ctx: unknown) => void): () => void {
      injects.push(callback)
      return () => undefined
    },
  }

  const registration = installPanel({ ctx, bridge: fakeBridge(), logger })
  assert.doesNotThrow(() => injects[0]!({}))
  assert.doesNotThrow(() => injects[0]!(undefined))
  assert.doesNotThrow(() => registration.dispose())
  assert.match(lines.at(-1)!.message, /appeared without a channel registry/)
})

test('the route is filed under the callback ctx, never the plugin ctx', () => {
  const { logger } = captureLogger()
  const registrations: Registration[] = []
  const routes: FakeRoute[] = []
  const { ctx, injects } = injectHost()
  const registration = installPanel({ ctx, bridge: fakeBridge(), logger })

  const reads: string[] = []
  injects[0]!.callback(
    fiberCtx({
      identity: 'inject-callback',
      declared: ['connection', 'webServer'],
      reads,
      services: { connection: connectionService(registrations), webServer: webServerService(routes) },
    }),
  )

  assert.equal(registrations.length, 1)
  assert.equal(registrations[0]!.owner, 'inject-callback')
  // Both halves of the registration happened on the callback context: the service
  // was read from it and the route was filed under it. Nothing was taken from the
  // plugin context and carried across.
  assert.deepEqual(
    reads.filter((name) => !name.startsWith('get:')),
    ['connection', 'webServer'],
  )
  registration.dispose()
})

test('a context that declared only connection is never asked for webServer', () => {
  const { logger, lines } = captureLogger()
  const registrations: Registration[] = []
  const { ctx, injects } = injectHost()
  installPanel({ ctx, bridge: fakeBridge(), logger })

  const reads: string[] = []
  injects[0]!.callback(
    fiberCtx({
      identity: 'inject-callback',
      declared: ['connection'],
      reads,
      // Records the owner and needs nothing else, so any webServer read recorded
      // here came from this plugin rather than from the host's registration.
      services: { connection: recordingConnectionService(registrations) },
    }),
  )

  assert.equal(registrations.length, 1)
  assert.equal(registrations[0]!.owner, 'inject-callback')
  assert.equal(reads.includes('webServer'), false)
  assert.equal(reads.includes('!webServer'), false)
  assert.deepEqual(
    lines.filter((line) => line.level === 'warn'),
    [],
  )
})

test('an owner that cannot reach webServer leaves the panel inert and says why', () => {
  const { logger, lines } = captureLogger()
  const registrations: Registration[] = []
  const { ctx, injects } = injectHost()
  const registration = installPanel({ ctx, bridge: fakeBridge(), logger })

  // The shipped service mounts the route on owner.webServer, so an owner holding
  // only `connection` makes that read throw. The plugin has to survive it and say
  // which piece is missing instead of taking the host down.
  assert.doesNotThrow(() => {
    injects[0]!.callback(
      fiberCtx({
        identity: 'inject-callback',
        declared: ['connection'],
        services: { connection: connectionService(registrations) },
      }),
    )
  })

  assert.equal(registrations.length, 0)
  const warn = lines.filter((line) => line.level === 'warn').at(-1)!
  assert.match(warn.message, /could not be registered once connection appeared/)
  assert.match((warn.details as Error).message, /cannot get property "webServer" without inject/)
  assert.doesNotThrow(() => registration.dispose())
})

test('the rpc.handle fallback is not consulted while the service can take an owner', () => {
  const { logger } = captureLogger()
  const registrations: Registration[] = []
  const routes: FakeRoute[] = []
  let rpcUsed = 0
  const { ctx, injects } = injectHost()
  installPanel({ ctx, bridge: fakeBridge(), logger })

  injects[0]!.callback(
    fiberCtx({
      identity: 'inject-callback',
      declared: ['connection', 'webServer'],
      services: {
        // Both entry points exist. rpc.handle picks its own owner and is broken on
        // DSH ("cannot get property webServer without inject"); register() takes the
        // owner we hand it, so that is the one that must be used.
        connection: connectionService(registrations, {
          onRpc: () => {
            rpcUsed += 1
          },
        }),
        webServer: webServerService(routes),
      },
    }),
  )

  assert.equal(rpcUsed, 0)
  assert.equal(registrations.length, 1)
  assert.equal(registrations[0]!.owner, 'inject-callback')
})

test('ctx.inject returning a fiber is reported as teardown, not as a leak', () => {
  const { logger, lines } = captureLogger()
  const registrations: Registration[] = []
  const routes: FakeRoute[] = []
  // Cordis 4 hands back the injected fiber, which is a thenable rather than a
  // disposer: the deferred callback dies with the fiber, so nothing can leak.
  const { ctx, injects } = injectHost({ then: () => undefined })
  const registration = installPanel({ ctx, bridge: fakeBridge(), logger })

  assert.equal(
    lines.some((line) => /leak/.test(line.message)),
    false,
  )
  const debugs = lines.filter((line) => line.level === 'debug').map((line) => line.message)
  assert.ok(debugs.some((message) => /torn down with its fiber/.test(message)))

  injects[0]!.callback(
    fiberCtx({
      identity: 'inject-callback',
      declared: ['connection', 'webServer'],
      services: { connection: connectionService(registrations), webServer: webServerService(routes) },
    }),
  )
  assert.equal(registrations.length, 1)
  assert.doesNotThrow(() => registration.dispose())
  assert.equal(registrations[0]!.released, 1)
})

test('ctx.inject refusing leaves the panel inert and does not propagate', () => {
  const { logger, lines } = captureLogger()
  const ctx = {
    get: () => undefined,
    inject(): () => void {
      throw new Error('inject refused')
    },
  }

  let registration: PanelRegistration | null = null
  assert.doesNotThrow(() => {
    registration = installPanel({ ctx, bridge: fakeBridge(), logger })
  })
  assert.doesNotThrow(() => registration!.dispose())
  assert.equal(lines.filter((line) => line.level === 'warn').length, 1)
  assert.match(lines[0]!.message, /ctx\.inject refused/)
})

test('with neither connection nor ctx.inject the warn says what is missing and what is not', () => {
  const { logger, lines } = captureLogger()
  for (const ctx of [undefined, {}, { get: () => undefined }, { get: () => ({}) }]) {
    installPanel({ ctx, bridge: fakeBridge(), logger }).dispose()
  }

  const warns = lines.filter((line) => line.level === 'warn')
  assert.equal(warns.length, 4)
  const message = warns[0]!.message
  assert.match(message, /no connection service/)
  assert.match(message, /\/farwork/)
  assert.match(message, /panel stays inert/)
  assert.match(message, /nothing is wrong with DSH/)
  assert.equal(lines.filter((line) => line.level === 'info').length, 0)
})

test('the bridge records why start failed instead of only logging it', async () => {
  const bridge = new Bridge({
    ctx: {},
    config: { endpointDir: '/dev/null/farwork-panel-test' },
    logger: NO_LOGGER(),
    pid: 4242,
  })
  assert.deepEqual(bridge.status, {
    ready: false,
    port: 0,
    endpointPath: '/dev/null/farwork-panel-test/endpoint.json',
    pid: 4242,
    lastError: null,
  })

  await assert.rejects(bridge.start())
  const status = bridge.status
  assert.equal(status.ready, false)
  assert.equal(typeof status.lastError, 'string')
  assert.ok((status.lastError ?? '').length > 0)
  await bridge.stop()
})
