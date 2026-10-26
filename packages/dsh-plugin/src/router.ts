// The method set, and nothing else.
//
// Every method here is one crates/farwork-agent actually calls, plus the two
// `session.getCapabilities`/runtime.getCapabilities` answers it branches on. Anything
// else is METHOD_NOT_FOUND on purpose: a method that exists but is half-implemented
// is worse than an honest -32601, because the client would believe the answer.

import { randomUUID } from 'node:crypto'
import { stat } from 'node:fs/promises'
import { isAbsolute } from 'node:path'

import { SyncFeed } from './feed.js'
import type { Operation } from './feed.js'
import { nativeSessionId, sessionId } from './identity.js'
import type { Logger } from './logger.js'
import { BridgeError, MAX_BATCH_BYTES, MAX_BATCH_ITEMS, PROJECTION_VERSION } from './protocol.js'
import { projectHistory } from './projection.js'
import type { SessionProjection, TimelineItem } from './projection.js'
import type { Host } from './runtime.js'
import type { HostChange, SessionEntry } from './runtime.js'
import type { AuthContext } from './transport.js'
import { asNumber, asString, bytesOf, nonEmptyString, record, timestampOf } from './values.js'
import type { Dict } from './values.js'

/** The closed capability vocabulary of the contract; ids we cannot serve report false. */
const CAPABILITY_IDS = [
  'runtime.config',
  'session.send_message',
  'session.interrupt',
  'session.steer',
  'session.interaction.approval',
  'catalog.model',
  'catalog.permission',
  'catalog.effort',
  'session.commands',
  'runtime.attachment',
] as const

const CAPABILITY_REVISION = 7
const PAGE_LIFETIME_MS = 5 * 60_000
const MAX_PAGE_ITEMS = 1_000

export interface FeedPlan {
  id: string
  notify: (method: string, params: unknown) => void
  onFailure: (error: Error) => void
}

export interface RouterOptions {
  host: Host
  logger: Logger
  notify: (method: string, params: unknown) => void
  /** Test seam: a feed with a fast clock instead of real timers. */
  feedFor?: (plan: FeedPlan) => SyncFeed
  newStreamId?: () => string
}

interface Inventory {
  id: string
  entries: SessionEntry[]
  expires: number
}

interface History {
  id: string
  platformId: string
  externalId: string
  items: TimelineItem[]
  truncated: boolean
  expires: number
}

export class RuntimeRouter {
  private readonly host: Host
  private readonly logger: Logger
  private readonly notify: (method: string, params: unknown) => void
  private readonly feedFor: ((plan: FeedPlan) => SyncFeed) | undefined
  private readonly newStreamId: () => string
  private namespace = 'farwork'
  private feed: SyncFeed | null = null
  private feedAbort: AbortController | null = null
  private readonly projections = new Map<string, SessionProjection>()
  private readonly inventories = new Map<string, Inventory>()
  private readonly histories = new Map<string, History>()

  constructor(options: RouterOptions) {
    this.host = options.host
    this.logger = options.logger
    this.notify = options.notify
    this.feedFor = options.feedFor
    this.newStreamId = options.newStreamId ?? (() => 'sync_' + randomUUID())
  }

  initialize(context: AuthContext): void {
    this.namespace = context.sessionNamespace
    this.logger.info('bridge initialized', {
      namespace: this.namespace.slice(0, 64),
      client: context.clientName,
      writable: !this.host.readOnly,
    })
  }

  async request(method: string, params: unknown, signal: AbortSignal): Promise<unknown> {
    signal.throwIfAborted()
    switch (method) {
      case 'ping':
        return { ok: true }
      case 'session.list':
        return this.list(params, signal)
      case 'session.getSnapshot':
        return this.snapshot(params, signal)
      case 'session.getState':
        return this.state(params, signal)
      case 'session.getCapabilities':
        return this.capabilities(params, true)
      case 'runtime.getCapabilities':
        return this.capabilities(params, false)
      case 'session.createAndStart':
        return this.send(params, signal, true)
      case 'session.startTurn':
        return this.send(params, signal, false)
      case 'session.interrupt':
        return this.interrupt(params, signal)
      case 'runtime.sync.subscribe':
        return this.subscribe(params)
      case 'runtime.sync.ack':
        return this.ack(params)
      // getNotices / respondInteraction stay unimplemented in v1: they are the one
      // part of the contract with no stable host API (see README).
      default:
        throw new BridgeError('METHOD_NOT_FOUND', 'The DSH bridge does not implement ' + method + '.')
    }
  }

  /** Feed one DSH bus change into the active stream, if there is one. */
  onHostChange(change: HostChange): void {
    const feed = this.feed
    if (!feed) return
    try {
      if (change.type === 'event') {
        this.onHostEvent(change.id, change.event, feed)
      } else if (change.type === 'status') {
        const agent = this.host.agent(change.id)
        if (!agent) return
        this.notifyState(feed, change.id, agent.status ?? 'idle')
      }
      // A session created or disposed after subscribe is picked up by the next
      // subscribe; v1 does not splice a half-built baseline into a live stream.
    } catch (error) {
      this.logger.warn('dropping a host change', error)
    }
  }

  close(): void {
    this.feedAbort?.abort('bridge closing')
    this.feedAbort = null
    this.feed?.close()
    this.feed = null
    this.projections.clear()
    this.inventories.clear()
    this.histories.clear()
  }

  // ------------------------------------------------------------------ methods

  private async list(params: unknown, signal: AbortSignal): Promise<unknown> {
    const dict = record(params)
    const limit = clamp(asNumber(dict.limit) ?? 100, 1, 1_000)
    const cursor = nonEmptyString(dict.cursor)
    let inventory: Inventory
    let offset = 0
    if (cursor !== undefined) {
      const parsed = this.readCursor(cursor)
      const cached = this.inventories.get(parsed.id)
      if (!cached || cached.expires < Date.now()) {
        throw new BridgeError('INVALID_PARAMS', 'The read cursor expired. Start a new read.')
      }
      inventory = cached
      offset = parsed.offset
    } else {
      if (this.host.query === undefined) {
        throw new BridgeError(
          'UNSUPPORTED_OPERATION',
          'DSH sessionQuery is unavailable, so this bridge cannot enumerate sessions.',
        )
      }
      const entries = await this.host.listSessions(signal)
      const archived = this.host.archived
      inventory = {
        id: randomUUID(),
        entries: entries.filter((entry) => !archived.has(entry.header.id)),
        expires: Date.now() + PAGE_LIFETIME_MS,
      }
      this.inventories.set(inventory.id, inventory)
    }
    const slice = inventory.entries.slice(offset, offset + limit)
    const sessions = slice.map((entry) => this.sessionRecord(entry))
    const next = offset + slice.length
    return {
      sessions,
      nextCursor: next < inventory.entries.length ? inventory.id + ':' + next : null,
    }
  }

  private async snapshot(params: unknown, signal: AbortSignal): Promise<unknown> {
    const dict = record(params)
    const cursor = nonEmptyString(dict.cursor)
    let page: History
    if (cursor !== undefined) {
      const parsed = this.readCursor(cursor)
      const cached = this.histories.get(parsed.id)
      if (!cached || cached.expires < Date.now()) {
        throw new BridgeError('INVALID_PARAMS', 'The read cursor expired. Start a new read.')
      }
      const requestedSession = nonEmptyString(dict.sessionId)
      const requestedExternal = nonEmptyString(dict.externalSessionId)
      if (requestedSession !== undefined && requestedSession !== cached.platformId) {
        throw new BridgeError('INVALID_PARAMS', 'The cursor belongs to a different session.')
      }
      if (requestedExternal !== undefined && requestedExternal !== cached.externalId) {
        throw new BridgeError('INVALID_PARAMS', 'The cursor belongs to a different session.')
      }
      page = cached
      const items = this.pageOf(page, parsed.offset)
      return this.snapshotResult(page, items, parsed.offset + items.length)
    }

    const nativeId = this.resolveNative(params)
    const log = await this.host.readLog(nativeId, signal)
    if (!log) {
      throw new BridgeError(
        'UNSUPPORTED_OPERATION',
        'DSH session history is unavailable: neither sessionQuery nor a live session answered.',
      )
    }
    const platformId = sessionId(this.namespace, nativeId)
    const projection = projectHistory(log.events, nativeId, platformId)
    const all = projection.items()
    const limit = clamp(asNumber(dict.limit) ?? Math.max(all.length, 1), 1, 1_000_000)
    const values = all.slice(-limit)
    page = {
      id: randomUUID(),
      platformId,
      externalId: nativeId,
      items: values,
      truncated: values.length < all.length,
      expires: Date.now() + PAGE_LIFETIME_MS,
    }
    this.histories.set(page.id, page)
    const items = this.pageOf(page, 0)
    return this.snapshotResult(page, items, items.length)
  }

  private snapshotResult(page: History, items: TimelineItem[], next: number): unknown {
    return {
      sessionId: page.platformId,
      externalSessionId: page.externalId,
      runtime: 'dsh',
      items,
      complete: next === 0 && page.truncated === false,
      snapshotComplete: !page.truncated,
      nextCursor: next < page.items.length ? page.id + ':' + next : null,
      watermark: { seq: -1, revision: 'projection-' + String(PROJECTION_VERSION) },
      metadata: {
        projectionVersion: PROJECTION_VERSION,
        totalItems: page.items.length,
        readOnly: this.host.readOnly,
      },
    }
  }

  private pageOf(page: History, offset: number): TimelineItem[] {
    const items: TimelineItem[] = []
    let bytes = 0
    for (const item of page.items.slice(offset, offset + MAX_PAGE_ITEMS)) {
      const size = bytesOf(item)
      if (size > MAX_BATCH_BYTES) {
        throw new BridgeError('FRAME_TOO_LARGE', 'One DSH history item exceeds the transport limit.')
      }
      if (items.length > 0 && bytes + size > MAX_BATCH_BYTES) break
      items.push(item)
      bytes += size
    }
    return items
  }

  private async state(params: unknown, signal: AbortSignal): Promise<unknown> {
    const nativeId = this.resolveNative(params, true)
    const agent = this.host.agent(nativeId)
    let status = agent?.status
    if (status === undefined) {
      const log = await this.host.readLog(nativeId, signal).catch(() => null)
      status = lastTurnStatus(log?.events ?? [])
    }
    return {
      runtime: 'dsh',
      sessionId: sessionId(this.namespace, nativeId),
      externalSessionId: nativeId,
      status,
      selections: {},
      metadata: {
        readOnly: this.host.readOnly,
        attached: agent !== undefined,
      },
    }
  }

  private capabilities(params: unknown, sessionScoped: boolean): unknown {
    const writable = this.host.controller !== undefined
    let platformId: string | undefined
    if (sessionScoped) {
      try {
        platformId = sessionId(this.namespace, this.resolveNative(params, true))
      } catch {
        // A capability answer does not depend on one session existing.
        platformId = undefined
      }
    }
    const enabled = new Set<string>(['runtime.config'])
    if (writable) {
      enabled.add('session.send_message')
      enabled.add('session.interrupt')
    }
    return {
      runtime: 'dsh',
      revision: CAPABILITY_REVISION,
      ...(platformId === undefined ? {} : { sessionId: platformId }),
      capabilities: CAPABILITY_IDS.map((capabilityId) => ({
        capabilityId,
        runtime: 'dsh',
        scope: platformId === undefined ? 'runtime' : 'session',
        supported: enabled.has(capabilityId),
        available: enabled.has(capabilityId),
        allowed: enabled.has(capabilityId),
        ...(enabled.has(capabilityId) ? {} : { unavailableReason: 'This DSH bridge does not serve this capability.' }),
      })),
      metadata: { readOnly: !writable },
    }
  }

  private async send(params: unknown, signal: AbortSignal, create: boolean): Promise<unknown> {
    const dict = record(params)
    const platformId = nonEmptyString(dict.sessionId)
    const content = asString(dict.content)
    const clientMessageId = nonEmptyString(dict.clientMessageId)
    if (platformId === undefined || content === undefined || clientMessageId === undefined) {
      throw new BridgeError('INVALID_PARAMS', 'Session ID, text and clientMessageId are required.')
    }
    const nativeId = nativeSessionId(this.namespace, platformId)
    try {
      if (create) {
        const cwd = nonEmptyString(dict.cwd)
        if (cwd === undefined || !isAbsolute(cwd)) {
          throw new BridgeError('INVALID_PARAMS', 'Choose an absolute workspace directory.')
        }
        const info = await stat(cwd).catch(() => null)
        if (!info || !info.isDirectory()) {
          throw new BridgeError('INVALID_PARAMS', 'The workspace must be an accessible directory on the DSH device.')
        }
        const preset = nonEmptyString(dict.agentPreset) ?? (await this.host.defaultAgentPreset())
        if (preset === undefined) {
          throw new BridgeError(
            'UNSUPPORTED_OPERATION',
            'No DSH Agent mode is available to create a session with.',
          )
        }
        await this.host.createSession({ sessionId: nativeId, cwd, agentPreset: preset })
        await this.host.createWorkspace(cwd, nativeId)
      }
      await this.host.resolveAgent(nativeId, signal)
      signal.throwIfAborted()
      await this.host.prompt({ sessionId: nativeId, requestId: clientMessageId, content }, signal)
      return { accepted: true, sessionId: platformId, externalSessionId: nativeId }
    } catch (error) {
      const failure =
        error instanceof BridgeError
          ? error
          : new BridgeError('DSH_SERVICE_UNAVAILABLE', error instanceof Error ? error.message : String(error), true)
      // A refused send is an answer, not a transport failure: the client reads
      // ok:false and reports the message instead of giving up on the session.
      return {
        ok: false,
        code: failure.code,
        message: failure.message,
        result: {
          sessionId: platformId,
          externalSessionId: nativeId,
          created: create,
          messageAccepted: false,
        },
      }
    }
  }

  private async interrupt(params: unknown, signal: AbortSignal): Promise<unknown> {
    const nativeId = this.resolveNative(params, true)
    signal.throwIfAborted()
    if (this.host.agents === undefined) {
      throw new BridgeError('UNSUPPORTED_OPERATION', 'Interrupt is unavailable: DSH has no agents service.')
    }
    const agent = this.host.agent(nativeId)
    if (!agent || typeof agent.cancel !== 'function') {
      throw new BridgeError('SESSION_NOT_FOUND', 'The DSH session is not attached to a live agent.')
    }
    agent.cancel({ kind: 'user' })
    return {
      accepted: true,
      sessionId: sessionId(this.namespace, nativeId),
      externalSessionId: nativeId,
    }
  }

  private subscribe(params: unknown): unknown {
    const dict = record(params)
    this.feedAbort?.abort('resubscribed')
    this.feed?.close()
    this.projections.clear()
    const id = this.newStreamId()
    const controller = new AbortController()
    this.feedAbort = controller
    const plan: FeedPlan = {
      id,
      notify: this.notify,
      onFailure: (error) => this.onStreamFailure(id, error),
    }
    const feed = this.feedFor ? this.feedFor(plan) : new SyncFeed(plan)
    this.feed = feed
    void this.baseline(controller.signal)
      .then((operations) => {
        if (this.feed === feed) feed.start(operations)
      })
      .catch((error) => this.onStreamFailure(id, error))
    // Neither checkpoint nor upload-progress is negotiated: v1 always sends a
    // full snapshot and never asks the client to resume from a checkpoint, so
    // advertising either extension would promise a recovery path that is not there.
    return { streamId: id, projectionVersion: PROJECTION_VERSION }
  }

  private ack(params: unknown): unknown {
    const dict = record(params)
    const feed = this.feed
    if (!feed) throw new BridgeError('INVALID_PARAMS', 'Unknown event stream.')
    if (asString(dict.streamId) !== feed.id) throw new BridgeError('INVALID_PARAMS', 'Unknown event stream.')
    if (asNumber(dict.batchSeq) === undefined) {
      throw new BridgeError('INVALID_PARAMS', 'Invalid event acknowledgement.')
    }
    feed.ack(dict.batchSeq)
    return { ok: true }
  }

  // ------------------------------------------------------------- stream source

  private async baseline(signal: AbortSignal): Promise<Operation[]> {
    const entries = await this.host.listSessions(signal)
    const archived = this.host.archived
    const operations: Operation[] = []
    for (const entry of entries) {
      signal.throwIfAborted()
      if (archived.has(entry.header.id)) continue
      const sessionOperations = await this.sessionBaseline(entry, signal)
      if (sessionOperations) operations.push(...sessionOperations)
    }
    return operations
  }

  private async sessionBaseline(entry: SessionEntry, signal: AbortSignal): Promise<Operation[] | null> {
    const nativeId = entry.header.id
    const log = await this.host.readLog(nativeId, signal)
    if (!log) return null
    const platformId = sessionId(this.namespace, nativeId)
    const projection = projectHistory(log.events, nativeId, platformId)
    this.projections.set(nativeId, projection)
    const items = projection.items()
    const snapshotId = randomUUID()
    const throughSeq = projection.throughSeq
    const operations: Operation[] = [
      {
        kind: 'snapshot.begin',
        sessionId: platformId,
        externalSessionId: nativeId,
        snapshotId,
        throughSeq,
        totalItems: items.length,
        meta: { externalSessionId: nativeId },
      },
    ]
    for (let index = 0; index < items.length; index += MAX_BATCH_ITEMS) {
      operations.push({
        kind: 'snapshot.items',
        sessionId: platformId,
        externalSessionId: nativeId,
        snapshotId,
        items: items.slice(index, index + MAX_BATCH_ITEMS),
      })
    }
    operations.push({
      kind: 'snapshot.commit',
      sessionId: platformId,
      externalSessionId: nativeId,
      snapshotId,
      totalItems: items.length,
      throughSeq,
    })
    operations.push({
      kind: 'notifications',
      notifications: [
        {
          method: 'session.meta.upsert',
          params: {
            sessionId: platformId,
            externalSessionId: nativeId,
            title: null,
            cwd: nonEmptyString(entry.header.cwd) ?? null,
            lastActivityAt: timestampOf(entry.header.createdAt),
          },
        },
        {
          method: 'session.state.updated',
          params: {
            sessionId: platformId,
            externalSessionId: nativeId,
            status: this.host.agent(nativeId)?.status ?? lastTurnStatus(log.events),
          },
        },
      ],
    })
    return operations
  }

  private onHostEvent(nativeId: string, event: { type: string; seq?: number; time?: string; data?: unknown }, feed: SyncFeed): void {
    const projection = this.projections.get(nativeId)
    if (!projection) return
    const platformId = sessionId(this.namespace, nativeId)
    let delta
    try {
      delta = projection.apply(event)
    } catch (error) {
      // A sequence gap means this projection can no longer be trusted; the client
      // has to resubscribe rather than receive items with invented positions.
      this.onStreamFailure(feed.id, error instanceof Error ? error : new Error(String(error)))
      return
    }
    const notifications: Operation[] = []
    for (const item of delta.upserted) {
      notifications.push({
        method: 'timeline.itemUpsert',
        params: { sessionId: platformId, externalSessionId: nativeId, item },
      })
    }
    if (event.type === 'turn/end') {
      const status = lastTurnStatus([event])
      notifications.push({
        method: 'session.state.updated',
        params: { sessionId: platformId, externalSessionId: nativeId, status },
      })
    }
    if (notifications.length === 0) return
    feed.enqueue([{ kind: 'notifications', notifications }])
  }

  private notifyState(feed: SyncFeed, nativeId: string, status: string): void {
    feed.enqueue([
      {
        kind: 'notifications',
        notifications: [
          {
            method: 'session.state.updated',
            params: {
              sessionId: sessionId(this.namespace, nativeId),
              externalSessionId: nativeId,
              status,
            },
          },
        ],
      },
    ])
  }

  private onStreamFailure(streamId: string, error: Error): void {
    this.logger.warn('event stream failed', { streamId, message: error.message })
    if (this.feed?.id === streamId) {
      // The stream is dead: stop pumping rather than leave a half-alive feed that
      // would keep numbering batches nobody reads.
      this.feed.close()
      this.feed = null
    }
    this.notify('runtime.error', {
      message: error.message,
      data: { scope: 'sync', streamId },
    })
  }

  // ------------------------------------------------------------------ helpers

  /**
   * Native session id for a request.
   *
   * Derivation, not enumeration: the native id is deterministic in the
   * namespace, so a lookup cannot disagree with what createAndStart created -
   * and the bridge does not need sessionQuery to answer a per-session call.
   */
  private resolveNative(params: unknown, requirePlatformId = false): string {
    const dict = record(params)
    const external = nonEmptyString(dict.externalSessionId)
    if (external !== undefined) {
      const claimed = nonEmptyString(dict.sessionId)
      if (claimed !== undefined && claimed !== sessionId(this.namespace, external)) {
        throw new BridgeError('INVALID_PARAMS', 'The session does not belong to this runtime namespace.')
      }
      return external
    }
    const platformId = nonEmptyString(dict.sessionId)
    if (platformId === undefined) {
      throw new BridgeError('INVALID_PARAMS', 'A session identity is required.')
    }
    if (requirePlatformId && !/^[\w-]{1,128}$/.test(platformId)) {
      throw new BridgeError('INVALID_PARAMS', 'Invalid platform session identity.')
    }
    return nativeSessionId(this.namespace, platformId)
  }

  private readCursor(cursor: string): { id: string; offset: number } {
    const separator = cursor.lastIndexOf(':')
    if (separator <= 0) throw new BridgeError('INVALID_PARAMS', 'The read cursor is malformed.')
    const offset = Number.parseInt(cursor.slice(separator + 1), 10)
    if (!Number.isSafeInteger(offset) || offset < 0) {
      throw new BridgeError('INVALID_PARAMS', 'The read cursor is malformed.')
    }
    return { id: cursor.slice(0, separator), offset }
  }

  private sessionRecord(entry: SessionEntry): Dict {
    const nativeId = entry.header.id
    return {
      runtime: 'dsh',
      sessionId: sessionId(this.namespace, nativeId),
      externalSessionId: nativeId,
      // v1 has no title service; null is the honest answer, not a made-up name.
      title: null,
      cwd: nonEmptyString(entry.header.cwd) ?? null,
      orderingTime: timestampOf(entry.header.createdAt),
      metadata: {
        live: entry.live === true,
        persisted: entry.persisted === true,
        parentSession: nonEmptyString(entry.header.parentSession) ?? null,
        origin: nonEmptyString(entry.header.origin) ?? null,
        readOnly: this.host.readOnly,
        sync: { requires_timeline_sync: true, changed: true },
      },
    }
  }
}

function clamp(value: number, low: number, high: number): number {
  return Math.min(Math.max(Math.trunc(value), low), high)
}

function lastTurnStatus(events: unknown[]): string {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const event = record(events[index])
    if (event.type !== 'turn/end') continue
    const kind = asString(record(record(event.data).reason).kind)
    return kind === 'error' ? 'error' : 'idle'
  }
  return 'idle'
}
