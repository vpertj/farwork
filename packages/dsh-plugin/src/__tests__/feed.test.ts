import assert from 'node:assert/strict'
import { test } from 'node:test'

import { SyncFeed } from '../feed.js'
import { BridgeError } from '../protocol.js'

interface Sent {
  method: string
  params: unknown
}

function harness(options: { ackTimeoutMs?: number } = {}) {
  const sent: Sent[] = []
  const failures: Error[] = []
  let clock = 1_000
  const feed = new SyncFeed({
    id: 'str_1',
    notify: (method, params) => sent.push({ method, params }),
    onFailure: (error) => failures.push(error),
    ackTimeoutMs: options.ackTimeoutMs ?? 5_000,
    now: () => clock,
    sleep: async (ms) => {
      clock += ms
    },
  })
  return {
    feed,
    sent,
    failures,
    advance: (ms: number) => {
      clock += ms
    },
    batches: () => sent.map((entry) => entry.params as { batchSeq: number; projectionVersion: number; operations: unknown[] }),
  }
}

const settle = async (): Promise<void> => {
  for (let index = 0; index < 5; index += 1) await new Promise((resolve) => setTimeout(resolve, 0))
}

test('batchSeq is strictly increasing from 1 and carries the projection version', async () => {
  const { feed, batches, sent } = harness()
  feed.start([{ kind: 'notifications', notifications: [] }])
  await settle()
  assert.equal(sent.length, 1)
  assert.equal(sent[0]?.method, 'runtime.sync.batch')
  assert.deepEqual(batches().map((batch) => batch.batchSeq), [1])
  assert.equal(batches()[0]?.projectionVersion, 3)

  feed.enqueue([{ kind: 'notifications', notifications: [] }])
  await settle()
  // Still one batch: the next one waits for the ack of the first.
  assert.equal(batches().length, 1)

  feed.ack(1)
  await settle()
  assert.deepEqual(batches().map((batch) => batch.batchSeq), [1, 2])
  feed.ack(2)
  await settle()
  assert.equal(batches().length, 2)
})

test('two batches are never closer than 34ms', async () => {
  const stamps: number[] = []
  const feed = new SyncFeed({
    id: 'str_gap',
    notify: () => stamps.push(Date.now()),
    onFailure: () => undefined,
    minGapMs: 34,
    ackTimeoutMs: 5_000,
  })
  feed.start([{ kind: 'a' }])
  await settle()
  assert.equal(stamps.length, 1)
  // Ack immediately: the second batch must still wait out the gap.
  feed.ack(1)
  feed.enqueue([{ kind: 'b' }])
  await new Promise((resolve) => setTimeout(resolve, 80))
  assert.equal(stamps.length, 2)
  assert.ok((stamps[1] ?? 0) - (stamps[0] ?? 0) >= 34, 'batches must be at least 34ms apart')
  feed.ack(2)
  feed.close()
})

test('an out-of-range acknowledgement is a client bug, not silence', async () => {
  const { feed } = harness()
  feed.start([{ kind: 'a' }])
  await settle()
  assert.throws(() => feed.ack(0), BridgeError)
  assert.throws(() => feed.ack(99), BridgeError)
  assert.throws(() => feed.ack('1'), BridgeError)
  // The valid one still works.
  feed.ack(1)
})

test('an unacknowledged batch fails the stream', async () => {
  const { feed, failures } = harness({ ackTimeoutMs: 10 })
  feed.start([{ kind: 'a' }])
  await new Promise((resolve) => setTimeout(resolve, 60))
  assert.equal(failures.length, 1)
  assert.match(failures[0]?.message ?? '', /not acknowledged/)
})

test('nothing is sent after close', async () => {
  const { feed, sent } = harness()
  feed.start([{ kind: 'a' }])
  await settle()
  feed.close()
  feed.enqueue([{ kind: 'b' }])
  await settle()
  assert.equal(sent.length, 1)
})
