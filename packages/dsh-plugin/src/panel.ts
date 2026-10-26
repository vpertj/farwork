// The browser panel's host surface: one RPC channel with three endpoints.
//
// The panel exists because a bridge that failed to start currently says so only in
// a log line the user cannot see. Two rules keep it harmless:
//
// 1. Nothing here can throw into DSH. installPanel() degrades to an inert panel
//    and the handler always answers with a { ok } envelope.
// 2. The endpoint token never leaves the process. It is an equivalent credential
//    for this runtime and the agent reads it from the 0600 endpoint file itself,
//    so the browser half has no reason to ever see it.
//
// The channel is /farwork, not /api: /api belongs to the Typert gateway and a
// second interceptor on it would be a collision, not a convention.

import { readFileSync } from 'node:fs'

import type { BridgeStatus } from './bridge.js'
import type { Logger } from './logger.js'
import { errorMessage, isRecord } from './values.js'

export const PANEL_CHANNEL = '/farwork'

/** Only the panel's read side; a fake of this shape is enough to test the handler. */
export interface PanelBridge {
  readonly status: BridgeStatus
  readonly endpointFile: string
}

export interface PanelOk {
  ok: true
  value: Record<string, unknown>
}

export interface PanelFailure {
  ok: false
  error: { code: string; message: string }
}

export type PanelReply = PanelOk | PanelFailure

/** The connection service hands (endpoint, payload, signal, peer); only the first two are read. */
export type PanelHandler = (endpoint: string, params: unknown) => Promise<PanelReply>

export interface PanelOptions {
  ctx: unknown
  bridge: PanelBridge
  logger: Logger
}

export interface PanelRegistration {
  dispose(): void
}

interface PairState {
  connectorToken: string | null
  hub: string | null
}

const MAX_TOKEN_LENGTH = 1024
const MAX_HUB_LENGTH = 512
const MAX_ERROR_LENGTH = 200
const PLACEHOLDER_TOKEN = '<connectorToken>'

/**
 * The one line a user is meant to act on when the panel never appears: it names
 * the missing host piece, says the panel is the only casualty, and rules out the
 * harness itself having gone wrong.
 */
const NO_CONNECTION_WARN =
  'the Farwork panel channel ' +
  PANEL_CHANNEL +
  ' is not registered: this host exposes no connection service (neither ctx.get("connection") nor ' +
  'ctx.connection gives an rpc.handle) and no ctx.inject to defer it, so the panel stays inert. ' +
  'The bridge itself is unaffected and nothing is wrong with DSH; the panel needs a host that provides ' +
  'the connection service.'

/**
 * Register the panel channel through the first route this host supports.
 *
 * Three tiers, in order, each guarded on its own:
 *   1. the connection service resolves right now, so register immediately;
 *   2. Cordis offers ctx.inject, so register the moment the service appears
 *      instead of staying inert for the whole run;
 *   3. neither exists, so say so in one actionable line.
 *
 * The disposer must be called by us: rpc.handle is owned by the *connection*
 * service's context, so it does not die with this plugin, and a second
 * registration of the same channel is refused as already-registered. That is why
 * every returned handle - and the inject subscription itself - is threaded into
 * the plugin's own stop chain.
 */
export function installPanel(options: PanelOptions): PanelRegistration {
  const logger = options.logger
  const state: PairState = { connectorToken: null, hub: null }
  const handler = createHandler(options.bridge, state, logger)

  let rpc: RpcLike | null = null
  try {
    rpc = resolveRpc(options.ctx)
  } catch (error) {
    logger.warn('the connection service probe failed; the Farwork panel is inert', error)
    rpc = null
  }
  if (rpc !== null) {
    try {
      const release = rpc.handle(PANEL_CHANNEL, handler)
      if (typeof release !== 'function') {
        logger.warn('the Farwork panel channel returned no disposer; it cannot be released on stop')
        return inert()
      }
      logger.info('the Farwork panel channel ' + PANEL_CHANNEL + ' is registered (mode: direct)')
      return registrationOf([release as () => void], logger)
    } catch (error) {
      logger.warn('the Farwork panel channel could not be registered; the panel is inert', error)
      return inert()
    }
  }

  let inject: InjectLike | null = null
  try {
    inject = resolveInject(options.ctx)
  } catch (error) {
    logger.warn('the ctx.inject probe failed; the Farwork panel is inert', error)
    inject = null
  }
  if (inject !== null) {
    try {
      return injectPanel(inject, handler, logger)
    } catch (error) {
      logger.warn('the Farwork panel could not be installed through ctx.inject', error)
      return inert()
    }
  }

  logger.warn(NO_CONNECTION_WARN, { channel: PANEL_CHANNEL })
  return inert()
}

/**
 * A host whose connection service is not up yet. ctx.inject defers the callback
 * until the service exists, which is the difference between a panel that appears
 * late and one that never appears.
 */
function injectPanel(inject: InjectLike, handler: PanelHandler, logger: Logger): PanelRegistration {
  const releases: Array<() => void> = []
  let registered = false
  try {
    const stop = inject(['connection'], (ctxWithConnection: unknown) => {
      try {
        const rpc = resolveRpc(ctxWithConnection)
        if (rpc === null) {
          logger.warn('the connection service appeared without rpc.handle; the Farwork panel is inert')
          return
        }
        const release = rpc.handle(PANEL_CHANNEL, handler)
        if (typeof release !== 'function') {
          logger.warn('the Farwork panel channel returned no disposer; it cannot be released on stop')
          return
        }
        releases.push(release as () => void)
        registered = true
        logger.info('the Farwork panel channel ' + PANEL_CHANNEL + ' is registered (mode: inject)')
      } catch (error) {
        // A late refusal is still not a reason to fail the plugin.
        logger.warn('the Farwork panel channel could not be registered once connection appeared', error)
      }
    })
    if (typeof stop === 'function') releases.push(stop as () => void)
    else logger.warn('ctx.inject returned no disposer; a late Farwork panel registration would leak')
  } catch (error) {
    logger.warn('ctx.inject refused the Farwork panel registration; the panel is inert', error)
    return inert()
  }
  if (!registered) {
    // Without this line a user who restarts and sees no panel has nothing to grep.
    logger.info('the Farwork panel channel ' + PANEL_CHANNEL + ' is waiting for connection (mode: inject)')
  }
  return registrationOf(releases, logger)
}

/** Both a late rpc.handle disposer and the inject subscription must be released, newest first. */
function registrationOf(releases: Array<() => void>, logger: Logger): PanelRegistration {
  let disposed = false
  return {
    dispose(): void {
      if (disposed) return
      disposed = true
      for (let index = releases.length - 1; index >= 0; index -= 1) {
        try {
          releases[index]!()
        } catch (error) {
          logger.warn('the Farwork panel channel did not release cleanly', error)
        }
      }
    },
  }
}

/** Endpoint dispatch. Exported for tests; production goes through installPanel. */
export function createHandler(bridge: PanelBridge, state: PairState, logger: Logger): PanelHandler {
  return async (endpoint, params) => {
    try {
      switch (endpoint) {
        case 'status':
          return { ok: true, value: statusValue(bridge) }
        case 'command':
          return { ok: true, value: commandValue(bridge, state, params) }
        case 'savePair':
          return savePairValue(state, params)
        default:
          return failure('farwork/unknown_endpoint', 'unknown endpoint: ' + clip(endpoint, 64))
      }
    } catch (error) {
      // A failing panel request must reach the browser as ok:false, not as a
      // broken channel; the panel is the one place the user can read a failure.
      logger.warn('the Farwork panel endpoint failed', {
        endpoint: clip(endpoint, 64),
        error: errorMessage(error),
      })
      return failure('farwork/panel_failed', clip(errorMessage(error), MAX_ERROR_LENGTH))
    }
  }
}

/** A field-by-field whitelist: spreading the bridge status would invite a token leak later. */
function statusValue(bridge: PanelBridge): Record<string, unknown> {
  const status = bridge.status
  return {
    ready: status.ready === true,
    port: typeof status.port === 'number' ? status.port : 0,
    endpointPath: status.endpointPath,
    pid: status.pid,
    lastError: status.lastError ?? null,
    version: packageVersion(),
  }
}

function commandValue(bridge: PanelBridge, state: PairState, params: unknown): Record<string, unknown> {
  const pasted = nonEmptyText(payloadOf(params).connectorToken)
  // A token typed into the panel wins for this one render and is not stored here:
  // persisting is what savePair is for, so the reads stay side-effect free.
  const token = pasted ?? state.connectorToken ?? PLACEHOLDER_TOKEN
  return { command: buildCommand(token, bridge.endpointFile) }
}

function savePairValue(state: PairState, params: unknown): PanelReply {
  const payload = payloadOf(params)
  const rawToken = payload.connectorToken
  const rawHub = payload.hub
  if (rawToken !== undefined && typeof rawToken !== 'string') {
    return failure('farwork/invalid_token', 'connectorToken must be a string')
  }
  if (rawHub !== undefined && typeof rawHub !== 'string') {
    return failure('farwork/invalid_hub', 'hub must be a string')
  }
  const token = typeof rawToken === 'string' ? rawToken.trim() : ''
  const hub = typeof rawHub === 'string' ? rawHub.trim() : ''
  if (token.length > MAX_TOKEN_LENGTH) {
    return failure('farwork/invalid_token', 'connectorToken is too long')
  }
  if (hub.length > MAX_HUB_LENGTH) {
    return failure('farwork/invalid_hub', 'hub is too long')
  }
  if (hub.length > 0 && !isHubUrl(hub)) {
    // Caught here so the panel can say why: the pasted value ends up in the command
    // line, and a bare host would fail on the device instead.
    return failure('farwork/invalid_hub', 'hub must start with http(s):// or ws(s)://')
  }
  if (token.length > 0) state.connectorToken = token
  if (hub.length > 0) state.hub = hub
  // Reports presence, never the secret: this reply is rendered in a browser.
  return { ok: true, value: { accepted: true, hasToken: state.connectorToken !== null, hub: state.hub } }
}

function buildCommand(token: string, endpointFile: string): string {
  return [
    'farwork-agent',
    '--token',
    shellQuote(token),
    '--runtime',
    'dsh',
    '--dsh-endpoint',
    shellQuote(endpointFile),
  ].join(' ')
}

function shellQuote(value: string): string {
  return "'" + value.replace(/'/g, "'\\''") + "'"
}

function isHubUrl(value: string): boolean {
  return /^(https?|wss?):\/\//i.test(value)
}

function payloadOf(params: unknown): Record<string, unknown> {
  if (!isRecord(params)) return {}
  // The connection service may hand over the request body rather than its params
  // member; accepting one level of nesting keeps both shapes working without a
  // second contract to keep in sync.
  return isRecord(params.params) ? params.params : params
}

function nonEmptyText(value: unknown): string | null {
  if (typeof value !== 'string') return null
  const trimmed = value.trim()
  return trimmed.length > 0 ? trimmed : null
}

function clip(value: unknown, limit: number): string {
  const text = typeof value === 'string' ? value : String(value)
  return text.length > limit ? text.slice(0, limit) + '...' : text
}

function failure(code: string, message: string): PanelFailure {
  return { ok: false, error: { code, message } }
}

function inert(): PanelRegistration {
  return { dispose: () => undefined }
}

interface RpcLike {
  handle(channel: string, handler: PanelHandler): unknown
}

/** Cordis' deferred-registration form: run the callback once these services exist. */
type InjectLike = (services: string[], callback: (ctx: unknown) => void) => unknown

/** The documented route is ctx.get('connection'); a direct property is accepted too. */
function resolveConnection(ctx: unknown): unknown {
  try {
    const direct = (ctx as { connection?: unknown } | null | undefined)?.connection
    if (isRecord(direct)) return direct
  } catch {
    // A guarded getter may throw; the documented route below is the fallback.
  }
  let get: unknown
  try {
    get = (ctx as { get?: unknown } | null | undefined)?.get
  } catch {
    return undefined
  }
  if (typeof get !== 'function') return undefined
  try {
    return (get as (name: string) => unknown).call(ctx, 'connection')
  } catch {
    return undefined
  }
}

/** Duck-typed probe: a linked plugin cannot import the connection service's types. */
function resolveRpc(ctx: unknown): RpcLike | null {
  const connection = resolveConnection(ctx)
  if (!isRecord(connection)) return null
  const rpc = connection.rpc
  if (!isRecord(rpc)) return null
  const handle = rpc.handle
  if (typeof handle !== 'function') return null
  const bound = handle as (channel: string, handler: PanelHandler) => unknown
  return {
    handle: (channel, handler) => bound.call(rpc, channel, handler),
  }
}

/**
 * ctx.inject is what turns "the connection service is not up yet" into a late
 * registration. Older hosts simply do not have it, hence the probe.
 */
function resolveInject(ctx: unknown): InjectLike | null {
  let candidate: unknown
  try {
    candidate = (ctx as { inject?: unknown } | null | undefined)?.inject
  } catch {
    return null
  }
  if (typeof candidate !== 'function') return null
  const inject = candidate as InjectLike
  return (services, callback) => inject.call(ctx, services, callback)
}

let cachedVersion: string | null = null

/** Read from the package manifest once; a plugin version that drifts from it is worse than 'unknown'. */
function packageVersion(): string {
  if (cachedVersion !== null) return cachedVersion
  try {
    const raw = readFileSync(new URL('../package.json', import.meta.url), 'utf8')
    const parsed: unknown = JSON.parse(raw)
    cachedVersion = isRecord(parsed) && typeof parsed.version === 'string' ? parsed.version : 'unknown'
  } catch {
    cachedVersion = 'unknown'
  }
  return cachedVersion
}
