// The only place that knows what a Cordis context looks like.
//
// Everything here is optional. DSH services come and go with the plugins the user
// enabled, and this bridge has to stay loaded when one of them is missing - so
// each access is a capability probe, and a missing service becomes a refusal at
// the method that needed it rather than a crash at load time.

import { BridgeError } from './protocol.js'
import type { DshEvent } from './projection.js'
import { asArray, asString, isRecord, nonEmptyString, record } from './values.js'
import type { Dict } from './values.js'

export interface SessionHeader extends Dict {
  id: string
  cwd?: string
  createdAt?: string
}

export interface SessionEntry {
  header: SessionHeader
  live?: boolean
  persisted?: boolean
}

export interface SessionLog {
  session?: unknown
  events: unknown[]
}

export interface AgentHandle {
  status?: string
  cancel(options: { kind: string }): void
}

export interface AgentResolution {
  agent?: AgentHandle
  error?: unknown
}

type Unknown = Record<string, unknown>

function callable(target: unknown, name: string): ((...args: unknown[]) => unknown) | undefined {
  if (!isRecord(target) && typeof target !== 'object') return undefined
  const value = (target as Unknown | undefined)?.[name]
  return typeof value === 'function' ? (value as (...args: unknown[]) => unknown).bind(target) : undefined
}

export class Host {
  private readonly context: unknown
  private readonly getService: (name: string) => unknown

  constructor(ctx: unknown) {
    this.context = ctx
    const context = isRecord(ctx) ? ctx : undefined
    const lookup = context?.get
    this.getService =
      typeof lookup === 'function'
        ? (name: string) => {
            try {
              const found = (lookup as (key: string) => unknown).call(ctx, name)
              if (found !== undefined && found !== null) return found
            } catch {
              // A throwing ctx.get() must not poison the whole adapter.
            }
            return context?.[name]
          }
        : (name: string) => (isRecord(ctx) ? ctx[name] : undefined)
  }

  service(name: string): unknown {
    return this.getService(name)
  }

  get sessions(): unknown {
    return this.service('sessions')
  }

  get query(): unknown {
    return this.service('sessionQuery')
  }

  get agents(): unknown {
    return this.service('agents')
  }

  get controller(): unknown {
    return this.service('sessionController')
  }

  get agentPresets(): unknown {
    return this.service('agentPresets')
  }

  get workspaces(): unknown {
    return this.service('workspaceRegistry')
  }

  get readOnly(): boolean {
    return this.controller === undefined
  }

  get archived(): Set<string> {
    const registry = this.workspaces
    const ids = isRecord(registry) ? registry.archivedSessionIds : undefined
    if (ids instanceof Set) return new Set([...ids].filter((id): id is string => typeof id === 'string'))
    return new Set(asArray(ids).filter((id): id is string => typeof id === 'string'))
  }

  liveSession(id: string): unknown {
    const get = callable(this.sessions, 'get')
    if (!get) return undefined
    try {
      return get(id)
    } catch {
      return undefined
    }
  }

  async listSessions(signal: AbortSignal): Promise<SessionEntry[]> {
    const list = callable(this.query, 'listSessions')
    if (!list) return []
    const raw = await list(signal)
    const entries: SessionEntry[] = []
    for (const item of asArray(raw)) {
      const id = headerId(item)
      if (id === undefined) continue
      const entry = record(item)
      const header = { ...record(entry.header), id } as SessionHeader & Dict
      entries.push({ header, live: entry.live === true, persisted: entry.persisted === true })
    }
    return entries
  }

  async readLog(id: string, signal: AbortSignal): Promise<SessionLog | null> {
    const live = this.liveSession(id)
    if (live !== undefined && live !== null) {
      const snapshot = callable(live, 'snapshotEvents')
      const header = isRecord(live) ? live.header : undefined
      if (snapshot) {
        const events = snapshot()
        return { session: { header }, events: asArray(events) }
      }
    }
    const read = callable(this.query, 'readSession')
    if (!read) return null
    const raw = await read(id, signal)
    if (!isRecord(raw)) return null
    return { session: raw.session, events: asArray(raw.events) }
  }

  agent(id: string): AgentHandle | undefined {
    const get = callable(this.agents, 'get')
    if (!get) return undefined
    const found = get(id)
    if (!isRecord(found)) return undefined
    return found as unknown as AgentHandle
  }

  async resolveAgent(id: string, signal: AbortSignal): Promise<AgentHandle> {
    const resolve = callable(this.controller, 'resolveAgent')
    if (!resolve) throw new BridgeError('UNSUPPORTED_OPERATION', 'the DSH session controller is unavailable')
    signal.throwIfAborted()
    const found = (await resolve(id)) as unknown
    const resolution = record(found) as unknown as AgentResolution
    if (resolution.error !== undefined || resolution.agent === undefined) {
      throw new BridgeError('DSH_SERVICE_UNAVAILABLE', 'DSH could not activate this session; retry when it is available', true)
    }
    return resolution.agent
  }

  async createSession(args: { sessionId: string; cwd: string; agentPreset: string }): Promise<void> {
    const create = callable(this.controller, 'create')
    if (!create) throw new BridgeError('UNSUPPORTED_OPERATION', 'the DSH session controller cannot create sessions')
    await create({ sessionId: args.sessionId, cwd: args.cwd, agentPreset: args.agentPreset })
  }

  async prompt(args: { sessionId: string; requestId: string; content: string }, signal: AbortSignal): Promise<void> {
    const prompt = callable(this.controller, 'prompt')
    if (!prompt) throw new BridgeError('UNSUPPORTED_OPERATION', 'the DSH session controller cannot queue messages')
    await prompt(
      {
        sessionId: args.sessionId,
        requestId: args.requestId,
        mode: 'queue',
        content: [{ type: 'text', text: args.content }],
      },
      signal,
    )
  }

  async defaultAgentPreset(): Promise<string | undefined> {
    const list = callable(this.agentPresets, 'list')
    if (!list) return undefined
    const raw = await list()
    for (const item of asArray(raw)) {
      const entry = record(item)
      const id = nonEmptyString(entry.id)
      if (id === undefined || entry.broken === true) continue
      return id
    }
    return undefined
  }

  async createWorkspace(path: string, sessionId: string): Promise<void> {
    const create = callable(this.workspaces, 'create')
    if (!create) return
    const workspace = await create(path)
    const attach = callable(workspace, 'attachSession')
    if (attach) await attach(sessionId)
  }
}

export function headerId(value: unknown): string | undefined {
  const entry = record(value)
  const header = record(entry.header ?? value)
  return asString(header.id) === undefined ? undefined : nonEmptyString(header.id)
}


export type HostChange =
  | { type: 'event'; id: string; event: DshEvent }
  | { type: 'status'; id: string }
  | { type: 'session'; id: string }
  | { type: 'refresh'; id: string }
  | { type: 'visibility' }

/**
 * Subscribe to the DSH event bus.
 *
 * Returns a disposer, or null when this host has no event bus at all - in that
 * case the bridge must not advertise an event stream it cannot feed.
 */
export function watchEvents(ctx: unknown, onChange: (change: HostChange) => void): (() => void) | null {
  if (!isRecord(ctx)) return null
  const on = ctx.on
  if (typeof on !== 'function') return null
  const subscribe = on as (event: string, handler: (...args: unknown[]) => void, options: { global: boolean }) => unknown
  const disposers: Array<() => void> = []
  const attach = (event: string, handler: (...args: unknown[]) => void): boolean => {
    try {
      const disposer = subscribe.call(ctx, event, handler, { global: true })
      if (typeof disposer === 'function') disposers.push(disposer as () => void)
      return true
    } catch {
      return false
    }
  }

  const installed = attach('session/event', (session, event) => {
    const id = headerId(session)
    if (id === undefined || !isRecord(event)) return
    const type = asString(event.type)
    if (type === undefined) return
    onChange({ type: 'event', id, event: event as unknown as DshEvent })
  })
  attach('session/created', (session) => {
    const id = headerId(session)
    if (id !== undefined) onChange({ type: 'session', id })
  })
  attach('session/disposed', (session) => {
    const id = headerId(session)
    if (id !== undefined) onChange({ type: 'session', id })
  })
  attach('agent/status', (payload) => {
    const agent = record(payload).agent
    const id = headerId(agent)
    if (id !== undefined) onChange({ type: 'status', id })
  })
  attach('domain/changed', (change) => {
    const dict = record(change)
    if (dict.domain !== 'workspace') return
    onChange({ type: 'visibility' })
  })

  if (!installed) return null
  return () => {
    for (const dispose of disposers) {
      try {
        dispose()
      } catch {
        // A disposal that throws must not stop the rest.
      }
    }
  }
}
