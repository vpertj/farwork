import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
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
}

/** A connection service of the shape rpc.handle needs, recording what it was given. */
function connectionService(registrations: Registration[]): unknown {
  return {
    rpc: {
      handle(channel: string, handler: PanelHandler): () => void {
        const entry: Registration = { channel, handler, released: 0 }
        registrations.push(entry)
        return () => {
          entry.released += 1
        }
      },
    },
  }
}

function fakeConnection(): { ctx: unknown; registrations: Registration[] } {
  const registrations: Registration[] = []
  return {
    ctx: { get: (name: string) => (name === 'connection' ? connectionService(registrations) : undefined) },
    registrations,
  }
}

interface LogLine {
  level: string
  message: string
}

function captureLogger(): { logger: Logger; lines: LogLine[] } {
  const lines: LogLine[] = []
  const at = (level: string) => (message: string): void => {
    lines.push({ level, message })
  }
  return {
    logger: { debug: at('debug'), info: at('info'), warn: at('warn'), error: at('error') },
    lines,
  }
}

function install(bridge: PanelBridge) {
  const { ctx, registrations } = fakeConnection()
  const registration = installPanel({ ctx, bridge, logger: NO_LOGGER() })
  assert.equal(registrations.length, 1)
  return { handler: registrations[0]!.handler, registration, registrations }
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

test('status reports the six panel fields and leaks no endpoint token', async () => {
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
  const { handler } = install(fakeBridge())
  const value = valueOf(await handler('command', {}))
  assert.equal(value.command, "farwork-agent --token '<connectorToken>' --runtime dsh --dsh-endpoint '" + ENDPOINT + "'")
})

test('a pasted token is used for one command and is not persisted by reading it', async () => {
  const { handler } = install(fakeBridge())
  const pasted = valueOf(await handler('command', { connectorToken: 'ct_pasted' }))
  assert.equal(pasted.command, "farwork-agent --token 'ct_pasted' --runtime dsh --dsh-endpoint '" + ENDPOINT + "'")

  const again = valueOf(await handler('command', {}))
  assert.equal(again.command, "farwork-agent --token '<connectorToken>' --runtime dsh --dsh-endpoint '" + ENDPOINT + "'")
})

test('savePair remembers the token so a later command uses it, without echoing it', async () => {
  const { handler } = install(fakeBridge())
  const saved = await handler('savePair', { connectorToken: 'ct_saved', hub: 'https://hub.example.test' })
  const value = valueOf(saved)
  assert.deepEqual(value, { accepted: true, hasToken: true, hub: 'https://hub.example.test' })
  assert.equal(JSON.stringify(value).includes('ct_saved'), false)

  const command = valueOf(await handler('command', {}))
  assert.equal(command.command, "farwork-agent --token 'ct_saved' --runtime dsh --dsh-endpoint '" + ENDPOINT + "'")
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
      rpc: {
        handle(channel: string, handler: PanelHandler): () => void {
          registrations.push({ channel, handler, released: 0 })
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
  // subscribed to the service instead.
  assert.equal(registrations.length, 0)
  assert.deepEqual(injects.map((entry) => entry.services), [['connection']])
  assert.match(lines.at(-1)!.message, /waiting for connection \(mode: inject\)/)

  injects[0]!.callback({ connection: connectionService(registrations) })

  assert.equal(registrations.length, 1)
  assert.equal(registrations[0]!.channel, PANEL_CHANNEL)
  const infos = lines.filter((line) => line.level === 'info')
  assert.match(infos.at(-1)!.message, /registered \(mode: inject\)/)
  const value = valueOf(await registrations[0]!.handler('status', {}))
  assert.equal(value.ready, true)

  // Both the late rpc.handle disposer and the injection itself are released once.
  registration.dispose()
  registration.dispose()
  assert.equal(registrations[0]!.released, 1)
  assert.deepEqual(injectReleases, ['inject'])
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
  assert.match(lines.at(-1)!.message, /appeared without rpc\.handle/)
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
