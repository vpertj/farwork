import assert from 'node:assert/strict'
import { test } from 'node:test'

import { SessionProjection, projectHistory } from '../projection.js'

function message(seq: number, text: string) {
  return {
    type: 'assistant/message',
    seq,
    time: '2026-01-01T00:00:0' + String(seq) + 'Z',
    data: { message: { id: 'msg_' + String(seq), content: [{ type: 'text', text }] } },
  }
}

test('an assistant text block becomes one message item', () => {
  const projection = new SessionProjection('aa_x_sess_1', 'sess_1')
  const delta = projection.apply(message(1, 'pong'))
  assert.equal(delta.upserted.length, 1)
  const item = delta.upserted[0]
  assert.ok(item)
  assert.equal(item.type, 'message')
  assert.equal(item.status, 'done')
  assert.equal(item.role, 'assistant')
  assert.equal(item.sessionId, 'sess_1')
  assert.equal(item.content.kind, 'markdown')
  assert.equal(item.content.text, 'pong')
  assert.equal(item.metadata.itemTime, '2026-01-01T00:00:01Z')
  assert.ok(item.id.startsWith('dsh_'))
})

test('a tool call and its result are the same item, running then settled', () => {
  const projection = new SessionProjection('ext_1', 'sess_1')
  projection.apply({ type: 'turn/start', seq: 1, time: 't1', data: { turn: 1 } })
  const call = projection.apply({
    type: 'tool/call',
    seq: 2,
    time: 't2',
    data: { callId: 'call_1', name: 'shell', arguments: { command: 'ls' } },
  })
  assert.equal(call.upserted.length, 1)
  const running = call.upserted[0]
  assert.ok(running)
  assert.equal(running.type, 'tool')
  assert.equal(running.status, 'running')
  assert.equal(running.content.toolName, 'shell')

  const result = projection.apply({
    type: 'tool/result',
    seq: 3,
    time: 't3',
    data: { callId: 'call_1', message: { toolCallId: 'call_1', content: [{ type: 'text', text: 'file.txt' }] } },
  })
  const settled = result.upserted[0]
  assert.ok(settled)
  assert.equal(settled.id, running.id)
  assert.equal(settled.status, 'done')
  assert.equal(settled.content.output, 'file.txt')
  assert.equal(settled.content.isError, false)
})

test('a failed tool result is marked failed instead of done', () => {
  const projection = new SessionProjection('ext_1', 'sess_1')
  projection.apply({ type: 'tool/call', seq: 1, time: 't1', data: { callId: 'call_2', name: 'shell', arguments: {} } })
  const delta = projection.apply({
    type: 'tool/result',
    seq: 2,
    time: 't2',
    data: { callId: 'call_2', error: { message: 'boom' }, message: { toolCallId: 'call_2', content: [] } },
  })
  assert.equal(delta.upserted[0]?.status, 'failed')
  assert.equal(delta.upserted[0]?.content.isError, true)
})

test('an interruption closes everything still running in the turn', () => {
  const projection = new SessionProjection('ext_1', 'sess_1')
  projection.apply({ type: 'turn/start', seq: 1, time: 't1', data: { turn: 1 } })
  projection.apply({ type: 'tool/call', seq: 2, time: 't2', data: { callId: 'call_3', name: 'shell', arguments: {} } })
  const closed = projection.apply({ type: 'turn/end', seq: 3, time: 't3', data: { reason: { kind: 'aborted' } } })
  assert.equal(closed.upserted.length, 1)
  assert.equal(closed.upserted[0]?.status, 'interrupted')
  // And the item really is the one that was running, not a new row.
  assert.equal(projection.items().length, 1)
})

test('an error turn end marks running items failed', () => {
  const projection = new SessionProjection('ext_1', 'sess_1')
  projection.apply({ type: 'turn/start', seq: 1, time: 't1', data: {} })
  projection.apply({ type: 'tool/call', seq: 2, time: 't2', data: { callId: 'c', name: 'x', arguments: {} } })
  const closed = projection.apply({ type: 'turn/end', seq: 3, time: 't3', data: { reason: { kind: 'error' } } })
  assert.equal(closed.upserted[0]?.status, 'failed')
})

test('the user echo and unknown event kinds produce no items', () => {
  const projection = new SessionProjection('ext_1', 'sess_1')
  const delta = projection.apply({
    type: 'user/message',
    seq: 1,
    time: 't1',
    data: { message: { id: 'm', content: [{ type: 'text', text: 'hello' }] } },
  })
  assert.deepEqual(delta, { upserted: [], removed: [] })
  const unknown = projection.apply({ type: 'compaction/start', seq: 2, time: 't2', data: {} })
  assert.deepEqual(unknown.upserted, [])
})

test('a sequence gap is an error, never a silently mis-numbered item', () => {
  const projection = new SessionProjection('ext_1', 'sess_1')
  projection.apply(message(1, 'one'))
  assert.throws(() => projection.apply(message(3, 'three')), /gap/)
})

test('ids are stable across replays of the same log', () => {
  const events = [message(1, 'one'), message(2, 'two')]
  const first = projectHistory(events, 'ext_1', 'sess_1').items().map((item) => item.id)
  const second = projectHistory(events, 'ext_1', 'sess_1').items().map((item) => item.id)
  assert.deepEqual(first, second)
  assert.equal(new Set(first).size, 2)
})

test('an unreadable log stops the replay instead of inventing the rest', () => {
  const projection = projectHistory([message(1, 'one'), { type: 'assistant/message', seq: 9, data: null }], 'ext_1', 'sess_1')
  assert.equal(projection.items().length, 1)
})
