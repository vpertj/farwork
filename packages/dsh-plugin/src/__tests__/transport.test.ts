import assert from 'node:assert/strict'
import { test } from 'node:test'

import { FrameReader, RuntimeServer, authenticate, identityPayload, tokensMatch } from '../transport.js'
import type { Router } from '../transport.js'
import { BridgeError, MAX_FRAME_BYTES } from '../protocol.js'
import { TOKEN, dial, initializeParams } from './helpers.js'

test('frames are split across chunks and CRLF is tolerated', () => {
  const reader = new FrameReader()
  assert.deepEqual(reader.feed(Buffer.from('{"a":1}')), { frames: [], rejected: [] })
  const batch = reader.feed(Buffer.from('\n{"b":2}\r\n'))
  assert.deepEqual(batch.frames, ['{"a":1}', '{"b":2}'])
  assert.deepEqual(reader.feed(Buffer.from('1\n2\n3\n')).frames, ['1', '2', '3'])
  // Blank lines are not frames.
  assert.deepEqual(reader.feed(Buffer.from('\n\n')).frames, [])
})

test('a frame past the ceiling is refused with and without a terminator', () => {
  const terminated = Buffer.concat([Buffer.alloc(MAX_FRAME_BYTES + 1, 0x61), Buffer.from('\n')])
  const first = new FrameReader().feed(terminated)
  assert.equal(first.frames.length, 0)
  assert.equal(first.rejected[0]?.reason, 'FRAME_TOO_LARGE')

  // Exactly at the ceiling is still a frame: the limit is inclusive.
  const atLimit = Buffer.concat([Buffer.alloc(MAX_FRAME_BYTES, 0x61), Buffer.from('\n')])
  const frames = new FrameReader().feed(atLimit)
  assert.equal(frames.rejected.length, 0)
  assert.equal(frames.frames[0]?.length, MAX_FRAME_BYTES)
})

test('an unended oversized frame is discarded instead of buffered forever', () => {
  const reader = new FrameReader()
  const batch = reader.feed(Buffer.alloc(MAX_FRAME_BYTES + 2, 0x61))
  assert.equal(batch.rejected[0]?.reason, 'FRAME_TOO_LARGE')
  // Back in sync: the next newline ends the discard, the following frame parses.
  const after = reader.feed(Buffer.from('\n{"ok":true}\n'))
  assert.deepEqual(after.frames, ['{"ok":true}'])
})

test('a non-UTF8 frame is refused rather than lossily decoded', () => {
  const batch = new FrameReader().feed(Buffer.from([0xff, 0xfe, 0x0a]))
  assert.equal(batch.frames.length, 0)
  assert.equal(batch.rejected[0]?.reason, 'NOT_UTF8')
})

test('the token comparison is exact', () => {
  assert.equal(tokensMatch(TOKEN, TOKEN), true)
  assert.equal(tokensMatch(TOKEN, TOKEN + 'x'), false)
  assert.equal(tokensMatch(TOKEN, ''), false)
})

test('initialize refuses a wrong token, a wrong runtime and a wrong protocol major', () => {
  assert.equal(authenticate(initializeParams(), TOKEN).sessionNamespace, 'farwork-conn_1')
  const cases: Array<[string, Record<string, unknown>]> = [
    ['token', initializeParams({ authToken: 'nope' })],
    ['protocol', initializeParams({ protocolVersion: '2.0' })],
    ['runtime', initializeParams({ runtime: 'codex' })],
    ['namespace', initializeParams({ sessionNamespace: '', connectorId: '' })],
  ]
  for (const [label, params] of cases) {
    assert.throws(() => authenticate(params, TOKEN), /./, label)
  }
  // 1.x minors stay compatible; connectorId stands in for a missing namespace,
  // which is what the contract allows a caller to send instead.
  const minor = authenticate(initializeParams({ protocolVersion: '1.7', sessionNamespace: undefined }), TOKEN)
  assert.equal(minor.sessionNamespace, 'conn_1')
})

test('the identity payload matches what the Rust client validates', () => {
  const payload = identityPayload({ events: true }) as {
    identity: Record<string, unknown>
    storage: Record<string, unknown>
    features: Record<string, unknown>
  }
  assert.equal(payload.identity.runtime, 'dsh')
  assert.equal(payload.identity.protocolVersion, '1.0')
  assert.equal(payload.storage.mode, 'dsh-native')
  assert.equal(payload.features.syncMode, 'events')
  assert.equal(payload.features.projectionVersion, 3)
  // v1 carries no interactions: advertising them would render dead buttons.
  assert.equal(payload.features.approval, false)
  assert.equal(payload.features.userQuestions, false)
  assert.equal(identityPayload({ events: false }).features.syncMode, 'polling')
})

async function startServer(overrides: { inFlightMax?: number; requestTimeoutMs?: number; events?: boolean } = {}) {
  const calls: Array<{ method: string; params: unknown }> = []
  let release: (() => void) | null = null
  const router: Router = {
    async request(method, params, signal) {
      calls.push({ method, params })
      if (method === 'hang') {
        await new Promise<void>((resolve) => {
          release = resolve
          signal.addEventListener('abort', () => resolve(), { once: true })
        })
        signal.throwIfAborted()
        return { ok: true }
      }
      if (method === 'echo') return params
      throw new BridgeError('METHOD_NOT_FOUND', 'nope')
    },
  }
  const server = new RuntimeServer({ token: TOKEN, router, ...overrides })
  const port = await server.listen()
  return { server, port, calls, releaseHanging: () => release?.() }
}

test('initialize then unknown method answers -32601 over a real socket', async () => {
  const { server, port } = await startServer()
  const client = await dial(port)
  try {
    client.send({ jsonrpc: '2.0', id: 1, method: 'initialize', params: initializeParams() })
    const init = await client.next()
    assert.equal(init.id, 1)
    const result = init.result as { identity: { runtime: string }; features: { syncMode: string } }
    assert.equal(result.identity.runtime, 'dsh')
    assert.equal(result.features.syncMode, 'events')

    client.send({ jsonrpc: '2.0', id: 'aa-2', method: 'session.getNotices', params: {} })
    const refusal = await client.next()
    assert.equal(refusal.id, 'aa-2')
    assert.equal(refusal.error?.code, -32601)
    assert.equal(refusal.error?.data?.code, 'METHOD_NOT_FOUND')
    assert.equal(refusal.result, undefined)
  } finally {
    client.close()
    await server.close()
  }
})

test('a wrong token is refused and the socket is closed without disclosing methods', async () => {
  const { server, port } = await startServer()
  const client = await dial(port)
  try {
    client.send({ jsonrpc: '2.0', id: 1, method: 'initialize', params: initializeParams({ authToken: 'bad' }) })
    const refusal = await client.next()
    assert.equal(refusal.error?.code, -32001)
    assert.equal(refusal.error?.data?.code, 'UNAUTHORIZED')
    await new Promise((resolve) => setTimeout(resolve, 30))
    assert.equal(client.closed, true)
  } finally {
    client.close()
    await server.close()
  }
})

test('no method but initialize is answered before authentication', async () => {
  const { server, port } = await startServer()
  const client = await dial(port)
  try {
    client.send({ jsonrpc: '2.0', id: 5, method: 'session.list', params: {} })
    const refusal = await client.next()
    assert.equal(refusal.error?.code, -32001)
  } finally {
    client.close()
    await server.close()
  }
})

test('the seventeenth concurrent request is refused instead of queued forever', async () => {
  const { server, port, releaseHanging } = await startServer({ inFlightMax: 4 })
  const client = await dial(port)
  try {
    client.send({ jsonrpc: '2.0', id: 0, method: 'initialize', params: initializeParams() })
    await client.next()
    for (let index = 1; index <= 4; index += 1) {
      client.send({ jsonrpc: '2.0', id: index, method: 'hang', params: {} })
    }
    client.send({ jsonrpc: '2.0', id: 99, method: 'echo', params: { too: 'many' } })
    const refusal = await client.next()
    assert.equal(refusal.id, 99)
    assert.equal(refusal.error?.data?.code, 'TOO_MANY_REQUESTS')
    releaseHanging()
  } finally {
    client.close()
    await server.close()
  }
})

test('a duplicate request id is refused while the first is still in flight', async () => {
  const { server, port, releaseHanging } = await startServer()
  const client = await dial(port)
  try {
    client.send({ jsonrpc: '2.0', id: 0, method: 'initialize', params: initializeParams() })
    await client.next()
    client.send({ jsonrpc: '2.0', id: 'aa-1', method: 'hang', params: {} })
    client.send({ jsonrpc: '2.0', id: 'aa-1', method: 'echo', params: {} })
    const refusal = await client.next()
    assert.equal(refusal.error?.data?.code, 'INVALID_PARAMS')
    releaseHanging()
  } finally {
    client.close()
    await server.close()
  }
})

test('a request past its deadline is answered with REQUEST_TIMEOUT', async () => {
  const { server, port } = await startServer({ requestTimeoutMs: 20 })
  const client = await dial(port)
  try {
    client.send({ jsonrpc: '2.0', id: 0, method: 'initialize', params: initializeParams() })
    await client.next()
    client.send({ jsonrpc: '2.0', id: 'aa-1', method: 'hang', params: {} })
    const refusal = await client.next()
    assert.equal(refusal.error?.data?.code, 'REQUEST_TIMEOUT')
    assert.equal(refusal.error?.data?.retryable, true)
  } finally {
    client.close()
    await server.close()
  }
})

test('$/cancelRequest aborts the addressed request and answers it as cancelled', async () => {
  const { server, port } = await startServer()
  const client = await dial(port)
  try {
    client.send({ jsonrpc: '2.0', id: 0, method: 'initialize', params: initializeParams() })
    await client.next()
    client.send({ jsonrpc: '2.0', id: 'aa-7', method: 'hang', params: {} })
    await new Promise((resolve) => setTimeout(resolve, 10))
    client.send({ jsonrpc: '2.0', method: '$/cancelRequest', params: { id: 'aa-7' } })
    const answer = await client.next()
    assert.equal(answer.id, 'aa-7')
    assert.equal(answer.error?.data?.code, 'REQUEST_CANCELLED')
  } finally {
    client.close()
    await server.close()
  }
})

test('a notification carrying no id is not answered', async () => {
  const { server, port } = await startServer()
  const client = await dial(port)
  try {
    client.send({ jsonrpc: '2.0', id: 0, method: 'initialize', params: initializeParams() })
    await client.next()
    client.send({ jsonrpc: '2.0', method: 'some.notification', params: {} })
    client.send({ jsonrpc: '2.0', id: 'aa-1', method: 'echo', params: { ping: true } })
    const answer = await client.next()
    assert.equal(answer.id, 'aa-1')
    assert.deepEqual(answer.result, { ping: true })
  } finally {
    client.close()
    await server.close()
  }
})
