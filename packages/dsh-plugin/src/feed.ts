// One event stream: a queue, a 34 ms gap, and one unacknowledged batch at a time.
//
// Each rule is load-bearing for the client, not politeness:
// * batchSeq starts at 1 and never skips - a consumer treats a gap as data loss
//   and resubscribes, so a numbering bug here looks like a network outage there;
// * a batch must be acked before the next one goes out, otherwise a fast producer
//   floods a slow socket and the only symptom is a truncated read much later;
// * an unacked batch is a dead stream, not a slow one: after the deadline the
//   whole stream fails so the client can resubscribe from a clean baseline.

import { BridgeError, MAX_BATCH_BYTES, MAX_BATCH_ITEMS, MIN_BATCH_GAP_MS, ACK_TIMEOUT_MS, PROJECTION_VERSION } from './protocol.js'
import { bytesOf, asSafeInteger } from './values.js'
import type { Dict } from './values.js'

export type Operation = Dict

export interface Batch {
  streamId: string
  batchSeq: number
  projectionVersion: number
  operations: Operation[]
}

export interface FeedOptions {
  id: string
  notify: (method: string, params: unknown) => void
  onFailure: (error: Error) => void
  minGapMs?: number
  ackTimeoutMs?: number
  maxBatchItems?: number
  maxBatchBytes?: number
  now?: () => number
  sleep?: (ms: number) => Promise<void>
}

interface Pending {
  batchSeq: number
  /** Settles when the client acks this batch, rejects on the deadline. */
  wait: Promise<void>
  resolve: () => void
  timer: NodeJS.Timeout | null
}

export class SyncFeed {
  readonly id: string
  private readonly notify: (method: string, params: unknown) => void
  private readonly onFailure: (error: Error) => void
  private readonly minGapMs: number
  private readonly ackTimeoutMs: number
  private readonly maxBatchItems: number
  private readonly maxBatchBytes: number
  private readonly now: () => number
  private readonly sleep: (ms: number) => Promise<void>
  private queue: Operation[] = []
  private sequence = 0
  private lastSentAt = Number.NEGATIVE_INFINITY
  private pending: Pending | null = null
  private pumping = false
  private closed = false

  constructor(options: FeedOptions) {
    this.id = options.id
    this.notify = options.notify
    this.onFailure = options.onFailure
    this.minGapMs = options.minGapMs ?? MIN_BATCH_GAP_MS
    this.ackTimeoutMs = options.ackTimeoutMs ?? ACK_TIMEOUT_MS
    this.maxBatchItems = options.maxBatchItems ?? MAX_BATCH_ITEMS
    this.maxBatchBytes = options.maxBatchBytes ?? MAX_BATCH_BYTES
    this.now = options.now ?? Date.now
    this.sleep = options.sleep ?? ((ms) => new Promise<void>((resolve) => setTimeout(resolve, ms)))
  }

  /** Batches sent so far; the client must observe exactly 1..sequence. */
  get batchSeq(): number {
    return this.sequence
  }

  get queued(): number {
    return this.queue.length
  }

  start(baseline: Operation[]): void {
    if (baseline.length > 0) this.queue.push(...baseline)
    void this.pump()
  }

  enqueue(operations: Operation[]): void {
    if (this.closed || operations.length === 0) return
    this.queue.push(...operations)
    void this.pump()
  }

  /** Accept one acknowledgement; out-of-range values are a client bug we report. */
  ack(batchSeq: unknown): void {
    const seq = asSafeInteger(batchSeq)
    if (seq === undefined || seq < 1 || seq > this.sequence) {
      throw new BridgeError('INVALID_PARAMS', 'Invalid event acknowledgement.')
    }
    const pending = this.pending
    if (!pending || pending.batchSeq !== seq) return
    if (pending.timer) clearTimeout(pending.timer)
    this.pending = null
    pending.resolve()
  }

  close(): void {
    this.closed = true
    this.queue = []
    const pending = this.pending
    this.pending = null
    if (pending?.timer) clearTimeout(pending.timer)
    pending?.resolve()
  }

  private async pump(): Promise<void> {
    if (this.pumping || this.closed) return
    this.pumping = true
    try {
      while (!this.closed && this.queue.length > 0) {
        const wait = this.minGapMs - (this.now() - this.lastSentAt)
        if (wait > 0) await this.sleep(wait)
        if (this.closed) return
        const operations = this.take()
        if (operations.length === 0) return
        const batch: Batch = {
          streamId: this.id,
          batchSeq: ++this.sequence,
          projectionVersion: PROJECTION_VERSION,
          operations,
        }
        this.lastSentAt = this.now()
        // Arm before sending and await after: a client may ack within the same
        // tick, and an ack that arrives before the wait exists would be lost.
        const pending = this.armAck(batch.batchSeq)
        this.pending = pending
        this.notify('runtime.sync.batch', batch)
        await this.awaitAck(pending)
      }
    } catch (error) {
      this.fail(error)
    } finally {
      this.pumping = false
    }
  }

  private take(): Operation[] {
    const taken: Operation[] = []
    let bytes = 0
    while (this.queue.length > 0 && taken.length < this.maxBatchItems) {
      const next = this.queue[0]
      if (next === undefined) break
      const size = bytesOf(next)
      if (taken.length > 0 && bytes + size > this.maxBatchBytes) break
      this.queue.shift()
      taken.push(next)
      bytes += size
    }
    return taken
  }

  private armAck(batchSeq: number): Pending {
    let resolveAck: () => void = () => undefined
    let rejectAck: (error: Error) => void = () => undefined
    const wait = new Promise<void>((resolve, reject) => {
      resolveAck = resolve
      rejectAck = reject
    })
    const pending: Pending = { batchSeq, wait, resolve: resolveAck, timer: null }
    const timer = setTimeout(() => {
      if (this.pending !== pending) return
      this.pending = null
      rejectAck(new Error('batch ' + batchSeq + ' was not acknowledged in time'))
    }, this.ackTimeoutMs)
    timer.unref?.()
    pending.timer = timer
    return pending
  }

  private async awaitAck(pending: Pending): Promise<void> {
    try {
      await pending.wait
    } catch (error) {
      const failure = error instanceof Error ? error : new Error(String(error))
      this.fail(failure)
      throw failure
    }
  }

  private fail(error: unknown): void {
    if (this.closed) return
    this.closed = true
    this.queue = []
    try {
      this.onFailure(error instanceof Error ? error : new Error(String(error)))
    } catch {
      // Reporting a dead stream must not throw out of the pump.
    }
  }
}
