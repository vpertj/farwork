// DSH events in, timeline items out - only the four kinds we actually render.
//
// A deliberate subset. DSH's history has turn markers, reasoning blocks, images,
// compaction markers, approval records and more; this projection keeps text,
// tool calls, tool results and interruption because those are the item kinds
// crates/farwork-agent renders today. Emitting the rest would be bytes the
// renderer drops, and pretending to a full projection v3 would make a future
// checkpoint scheme lie about what it replayed.

import { itemId } from './identity.js'
import { asArray, asNumber, asString, isRecord, record, timestampOf } from './values.js'
import type { Dict } from './values.js'

export interface DshEvent {
  type: string
  seq?: number
  time?: string
  data?: unknown
}

export type ItemStatus = 'running' | 'done' | 'failed' | 'interrupted'

export interface TimelineSource {
  runtime: 'dsh'
  sessionId: string
  itemId: string
  itemType: string
  seq: number
  lastSeq: number
  time: string
}

export interface TimelineItem {
  id: string
  sessionId: string
  type: 'message' | 'tool'
  status: ItemStatus
  role: 'assistant'
  turnId: string | null
  orderSeq: number
  revision: number
  content: Dict
  source: TimelineSource
  /**
   * Not part of DSH's own item: our client reads metadata.itemTime for the row's
   * timestamp and would otherwise fall back to "now" for every item it replays.
   */
  metadata: { itemTime: string }
}

export interface ProjectionDelta {
  upserted: TimelineItem[]
  removed: string[]
}

function statusOf(reason: unknown): ItemStatus {
  const kind = asString(record(reason).kind) ?? ''
  if (kind === 'error') return 'failed'
  if (kind === 'aborted' || kind === 'interrupted') return 'interrupted'
  return 'done'
}

export class SessionProjection {
  private readonly externalId: string
  private readonly platformId: string
  private readonly itemsById = new Map<string, TimelineItem>()
  private order = 0
  private seq = -1
  private turnId: string | null = null

  constructor(externalId: string, platformId: string) {
    this.externalId = externalId
    this.platformId = platformId
  }

  get throughSeq(): number {
    return this.seq
  }

  items(): TimelineItem[] {
    return [...this.itemsById.values()].sort((left, right) => left.orderSeq - right.orderSeq || left.id.localeCompare(right.id))
  }

  /** Apply one history event; returns what changed, empty for unhandled kinds. */
  apply(event: DshEvent): ProjectionDelta {
    const seq = asNumber(event.seq) ?? this.seq + 1
    // A gap means events were lost; carrying on would place later rows at a
    // sequence they never had, so the projection refuses instead of guessing.
    if (this.seq >= 0 && seq !== this.seq + 1) {
      throw new Error('DSH event sequence gap: expected ' + (this.seq + 1) + ', got ' + seq)
    }
    this.seq = seq
    const data = record(event.data)
    const time = timestampOf(event.time)

    switch (event.type) {
      case 'turn/start': {
        this.turnId = itemId(this.externalId, 'turn', String(seq))
        return EMPTY
      }
      case 'turn/end': {
        const status = statusOf(data.reason)
        const closed = this.closeRunning(status === 'done' ? 'interrupted' : status, seq, time)
        this.turnId = null
        return { upserted: closed, removed: [] }
      }
      case 'assistant/message': {
        const message = record(data.message)
        const interrupted = data.interrupted === true
        const upserted: TimelineItem[] = []
        for (const [index, block] of asArray(message.content).entries()) {
          const value = record(block)
          const kind = asString(value.type)
          if (kind === 'text') {
            const item = this.put('message', 'assistant:' + String(message.id ?? seq) + ':' + String(index), event, {
              type: 'message',
              status: interrupted ? 'interrupted' : 'done',
              content: { kind: 'markdown', format: 'markdown', text: asString(value.text) ?? '' },
            })
            if (item) upserted.push(item)
          } else if (kind === 'tool-call') {
            const callId = asString(value.id)
            if (!callId) continue
            const item = this.tool(callId, asString(value.name) ?? 'tool', value.arguments, event)
            upserted.push(item)
          }
          // reasoning / image / unknown blocks are outside v1's four kinds.
        }
        return { upserted, removed: [] }
      }
      case 'tool/call': {
        const callId = asString(data.callId)
        if (!callId) return EMPTY
        return { upserted: [this.tool(callId, asString(data.name) ?? 'tool', data.arguments, event)], removed: [] }
      }
      case 'tool/result': {
        const message = record(data.message)
        const callId = asString(message.toolCallId) ?? asString(message.callId)
        if (!callId) return EMPTY
        const failed = data.error !== undefined || message.isError === true
        const previous = this.itemsById.get(itemId(this.externalId, 'tool', callId))
        const content: Dict = {
          ...(previous?.content ?? this.toolContent('tool', undefined)),
          callId,
          isError: failed,
          output: textOf(asArray(message.content)),
          ...(data.error !== undefined ? { error: data.error } : {}),
        }
        const item = this.put('tool', callId, event, {
          type: 'tool',
          status: failed ? 'failed' : 'done',
          content,
        })
        return item ? { upserted: [item], removed: [] } : EMPTY
      }
      default:
        return EMPTY
    }
  }

  private tool(callId: string, name: string, args: unknown, event: DshEvent): TimelineItem {
    const previous = this.itemsById.get(itemId(this.externalId, 'tool', callId))
    const item = this.put('tool', callId, event, {
      type: 'tool',
      status: previous?.status === 'running' || previous === undefined ? 'running' : previous.status,
      content: { ...(previous?.content ?? {}), ...this.toolContent(name, args), callId },
    })
    // put returns the item it wrote; the fallback cannot happen for a live key.
    return item ?? this.itemsById.get(itemId(this.externalId, 'tool', callId))!
  }

  private toolContent(name: string, args: unknown): Dict {
    return {
      kind: 'tool',
      toolName: name,
      ...(args === undefined ? {} : { arguments: args }),
    }
  }

  private closeRunning(status: ItemStatus, seq: number, time: string): TimelineItem[] {
    const upserted: TimelineItem[] = []
    for (const [id, item] of this.itemsById) {
      if (item.status !== 'running' || item.turnId !== this.turnId) continue
      const closed: TimelineItem = {
        ...item,
        status,
        revision: seq + 1,
        source: { ...item.source, lastSeq: seq },
        metadata: { itemTime: time },
      }
      this.itemsById.set(id, closed)
      upserted.push(closed)
    }
    return upserted
  }

  private put(
    kind: string,
    key: string,
    event: DshEvent,
    fields: { type: 'message' | 'tool'; status: ItemStatus; content: Dict },
  ): TimelineItem | null {
    const id = itemId(this.externalId, kind, key)
    const previous = this.itemsById.get(id)
    const time = timestampOf(event.time)
    const item: TimelineItem = {
      id,
      sessionId: this.platformId,
      type: fields.type,
      status: fields.status,
      role: 'assistant',
      turnId: previous?.turnId ?? this.turnId,
      orderSeq: previous?.orderSeq ?? ++this.order,
      revision: this.seq + 1,
      content: fields.content,
      source: {
        runtime: 'dsh',
        sessionId: this.externalId,
        itemId: key,
        itemType: event.type,
        seq: previous?.source.seq ?? this.seq,
        lastSeq: this.seq,
        time,
      },
      metadata: { itemTime: time },
    }
    this.itemsById.set(id, item)
    return item
  }
}

const EMPTY: ProjectionDelta = { upserted: [], removed: [] }

function textOf(blocks: unknown[]): string {
  const parts: string[] = []
  for (const block of blocks) {
    const value = record(block)
    const text = asString(value.text)
    if (text !== undefined) parts.push(text)
    else if (typeof block === 'string') parts.push(block)
  }
  return parts.join('')
}

/** Project a whole event log; used by both snapshot reads and the sync baseline. */
export function projectHistory(events: unknown[], externalId: string, platformId: string): SessionProjection {
  const projection = new SessionProjection(externalId, platformId)
  for (const raw of events) {
    if (!isRecord(raw)) continue
    const type = asString(raw.type)
    if (type === undefined) continue
    const event: DshEvent = {
      type,
      seq: asNumber(raw.seq),
      time: asString(raw.time),
      data: raw.data,
    }
    try {
      projection.apply(event)
    } catch {
      // A malformed log is not a reason to refuse the session: stop replaying at
      // the damage instead of fabricating the rest of the history.
      break
    }
  }
  return projection
}
