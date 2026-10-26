import assert from 'node:assert/strict'
import { test } from 'node:test'

import { SyncFeed } from '../feed.js'
import { NO_LOGGER } from '../logger.js'
import { nativeSessionId, sessionId } from '../identity.js'
import { BridgeError } from '../protocol.js'
import { RuntimeRouter } from '../router.js'
import { Host } from '../runtime.js'

const NAMESPACE = 'farwork-conn_1'
const PLATFORM_ID = 'sess_hub_1'
const NATIVE_ID = nativeSessionId(NAMESPACE, PLATFORM_ID)

interface Harness {
  router: RuntimeRouter
  pushes: Array<{ method: string; params: unknown }>
  prompts: Array<Record<string, unknown>>
  created: Array<Record<string, unknown>>
  cancels: Array<{ kind: string }>
  ctx: Record<string, unknown>
}

function harness(overrides: { sessions?: Record<string, unknown>; readOnly?: boolean } = {}): Harness {
  const prompts: Array<Record<string, unknown>> = []
  const created: Array<Record<string, unknown>> = []
  const cancels: Array<{ kind: string }> = []
  const pushes: Array<{ method: string; params: unknown }> = []
  const events = [
    { type: 'turn/start', seq: 1, time: '2026-01-01T00:00:01Z', data: { turn: 1 } },
    {
      type: 'assistant/message',
      seq: 2,
      time: '2026-01-01T00:00:02Z',
      data: { message: { id: 'm1', content: [{ type: 'text', text: 'pong' }] } },
    },
  ]
  const services: Record<string, unknown> = {
    sessions: overrides.sessions ?? {
      get: (id: string) => (id === NATIVE_ID ? { header: { id: NATIVE_ID }, snapshotEvents: () => events } : undefined),
    },
    sessionQuery: {
      listSessions: async () => [
        {
          header: { id: NATIVE_ID, cwd: '/tmp', createdAt: '2026-01-01T00:00:00Z' },
          live: true,
          persisted: true,
        },
      ],
      readSession: async () => ({ session: { header: { id: NATIVE_ID } }, events }),
    },
    agents: {
      get: (id: string) =>
        id === NATIVE_ID
          ? {
              status: 'idle',
              cancel: (options: { kind: string }) => {
                cancels.push(options)
              },
            }
          : undefined,
    },
    sessionController: overrides.readOnly === true ? undefined : {
      resolveAgent: async () => ({ agent: { status: 'idle' } }),
      create: async (args: Record<string, unknown>) => {
        created.push(args)
      },
      prompt: async (args: Record<string, unknown>) => {
        prompts.push(args)
      },
    },
    agentPresets: { list: async () => [{ id: 'ptc' }] },
    workspaceRegistry: { archivedSessionIds: [], create: async () => ({ attachSession: async () => undefined }) },
  }
  const ctx: Record<string, unknown> = {
    ...services,
    get: (name: string) => services[name],
  }
  const router = new RuntimeRouter({
    host: new Host(ctx),
    logger: NO_LOGGER(),
    notify: (method, params) => pushes.push({ method, params }),
    feedFor: (plan) => new SyncFeed({ ...plan, minGapMs: 0, ackTimeoutMs: 2_000 }),
  })
  router.initialize({
    connectorId: 'conn_1',
    sessionNamespace: NAMESPACE,
    protocolVersion: '1.0',
    clientName: 'farwork-agent',
    clientVersion: '0.0.0',
  })
  return { router, pushes, prompts, created, cancels, ctx }
}

function signal(): AbortSignal {
  return new AbortController().signal
}

async function settle(): Promise<void> {
  for (let index = 0; index < 6; index += 1) await new Promise((resolve) => setTimeout(resolve, 0))
}

test('ping answers ok', async () => {
  const { router } = harness()
  assert.deepEqual(await router.request('ping', {}, signal()), { ok: true })
})

test('methods outside the v1 set are METHOD_NOT_FOUND, including interactions', async () => {
  const { router } = harness()
  for (const method of ['session.getNotices', 'session.respondInteraction', 'session.executeCommand', 'runtime.sync.progress']) {
    await assert.rejects(
      () => router.request(method, {}, signal()),
      (error: unknown) => error instanceof BridgeError && error.code === 'METHOD_NOT_FOUND',
      method,
    )
  }
})

test('session.list reports both identities and never invents a title', async () => {
  const { router } = harness()
  const result = (await router.request('session.list', { limit: 5 }, signal())) as {
    sessions: Array<Record<string, unknown>>
    nextCursor: string | null
  }
  assert.equal(result.sessions.length, 1)
  const session = result.sessions[0]
  assert.equal(session?.sessionId, PLATFORM_ID)
  assert.equal(session?.externalSessionId, NATIVE_ID)
  assert.equal(session?.title, null)
  assert.equal(session?.runtime, 'dsh')
  assert.equal(result.nextCursor, null)
  // The platform id is what every later call sends, and it round-trips.
  assert.equal(sessionId(NAMESPACE, NATIVE_ID), PLATFORM_ID)
})

test('session.getSnapshot projects the log the client renders', async () => {
  const { router } = harness()
  const result = (await router.request('session.getSnapshot', { sessionId: PLATFORM_ID }, signal())) as {
    items: Array<Record<string, unknown>>
    metadata: { projectionVersion: number }
    snapshotComplete: boolean
  }
  assert.equal(result.metadata.projectionVersion, 3)
  assert.equal(result.items.length, 1)
  assert.equal(result.items[0]?.type, 'message')
  assert.equal(result.snapshotComplete, true)
})

test('session.getState reads the live status and admits when nothing is attached', async () => {
  const { router } = harness()
  const attached = (await router.request('session.getState', { sessionId: PLATFORM_ID }, signal())) as Record<string, unknown>
  assert.equal(attached.status, 'idle')
  assert.deepEqual(attached.metadata, { readOnly: false, attached: true })

  const detached = (await router.request('session.getState', { externalSessionId: NATIVE_ID }, signal())) as Record<string, unknown>
  const metadata = detached.metadata as { attached: boolean }
  assert.equal(detached.status, 'idle')
  assert.equal(metadata.attached, true)
})

test('capabilities report only what this bridge really serves', async () => {
  const { router } = harness()
  const runtime = (await router.request('runtime.getCapabilities', {}, signal())) as {
    revision: number
    capabilities: Array<{ capabilityId: string; supported: boolean; unavailableReason?: string }>
  }
  assert.equal(runtime.revision, 7)
  const enabled = new Set(runtime.capabilities.filter((entry) => entry.supported).map((entry) => entry.capabilityId))
  assert.deepEqual([...enabled].sort(), ['runtime.config', 'session.interrupt', 'session.send_message'])
  for (const unsupported of ['session.steer', 'session.interaction.approval', 'session.commands', 'catalog.model']) {
    const entry = runtime.capabilities.find((candidate) => candidate.capabilityId === unsupported)
    assert.equal(entry?.supported, false, unsupported)
    assert.ok(entry?.unavailableReason)
  }
  const sessionCaps = (await router.request('session.getCapabilities', { sessionId: PLATFORM_ID }, signal())) as {
    sessionId: string
    capabilities: Array<{ scope: string }>
  }
  assert.equal(sessionCaps.sessionId, PLATFORM_ID)
  assert.equal(sessionCaps.capabilities[0]?.scope, 'session')
})

test('a read-only host reports no messaging and no interrupt', async () => {
  const { router } = harness({ readOnly: true })
  const runtime = (await router.request('runtime.getCapabilities', {}, signal())) as {
    capabilities: Array<{ capabilityId: string; supported: boolean }>
    metadata: { readOnly: boolean }
  }
  const supported = runtime.capabilities.filter((entry) => entry.supported).map((entry) => entry.capabilityId)
  assert.deepEqual(supported, ['runtime.config'])
  assert.equal(runtime.metadata.readOnly, true)
  // And the write paths refuse instead of pretending.
  const result = (await router.request(
    'session.startTurn',
    { sessionId: PLATFORM_ID, content: 'hi', clientMessageId: 'cmid_9' },
    signal(),
  )) as { ok: boolean; code: string }
  assert.equal(result.ok, false)
  assert.equal(result.code, 'UNSUPPORTED_OPERATION')
})

test('createAndStart creates the native id deterministically and queues the text', async () => {
  const { router, created, prompts } = harness()
  const result = await router.request(
    'session.createAndStart',
    { sessionId: PLATFORM_ID, content: 'hello', clientMessageId: 'cmid_1', cwd: '/tmp' },
    signal(),
  )
  assert.deepEqual(result, { accepted: true, sessionId: PLATFORM_ID, externalSessionId: NATIVE_ID })
  assert.deepEqual(created, [{ sessionId: NATIVE_ID, cwd: '/tmp', agentPreset: 'ptc' }])
  assert.deepEqual(prompts, [
    { sessionId: NATIVE_ID, requestId: 'cmid_1', mode: 'queue', content: [{ type: 'text', text: 'hello' }] },
  ])
})

test('startTurn does not create anything and sends only the message', async () => {
  const { router, created, prompts } = harness()
  await router.request(
    'session.startTurn',
    { sessionId: PLATFORM_ID, externalSessionId: NATIVE_ID, content: 'again', clientMessageId: 'cmid_2' },
    signal(),
  )
  assert.deepEqual(created, [])
  assert.equal(prompts.length, 1)
  assert.equal(prompts[0]?.requestId, 'cmid_2')
})

test('a refused send answers ok:false with the reason instead of a transport error', async () => {
  const { router } = harness()
  const result = (await router.request(
    'session.createAndStart',
    { sessionId: PLATFORM_ID, content: 'hi', clientMessageId: 'cmid_3', cwd: 'relative/path' },
    signal(),
  )) as { ok: boolean; code: string; message: string }
  assert.equal(result.ok, false)
  assert.equal(result.code, 'INVALID_PARAMS')
  assert.match(result.message, /absolute/)
})

test('a session identity from another namespace is refused', async () => {
  const { router } = harness()
  await assert.rejects(
    () => router.request('session.getState', { sessionId: 'sess_other', externalSessionId: NATIVE_ID }, signal()),
    (error: unknown) => error instanceof BridgeError && error.code === 'INVALID_PARAMS',
  )
})

test('interrupt cancels the agent, and says so when there is none', async () => {
  const { router, cancels } = harness()
  const result = (await router.request('session.interrupt', { sessionId: PLATFORM_ID }, signal())) as Record<string, unknown>
  assert.equal(result.accepted, true)
  assert.deepEqual(cancels, [{ kind: 'user' }])

  await assert.rejects(
    () => router.request('session.interrupt', { sessionId: 'sess_missing' }, signal()),
    (error: unknown) => error instanceof BridgeError && error.code === 'SESSION_NOT_FOUND',
  )
})

test('subscribe starts a stream with a snapshot baseline the client can stage', async () => {
  const { router, pushes } = harness()
  const result = (await router.request('runtime.sync.subscribe', { checkpointVersion: 1, uploadProgressVersion: 1 }, signal())) as {
    streamId: string
    projectionVersion: number
  } & Record<string, unknown>
  assert.ok(result.streamId.startsWith('sync_'))
  assert.equal(result.projectionVersion, 3)
  // v1 negotiates neither extension, so it must not echo them.
  assert.equal(result.checkpointVersion, undefined)
  assert.equal(result.uploadProgressVersion, undefined)

  await settle()
  const batch = pushes.find((entry) => entry.method === 'runtime.sync.batch')?.params as {
    streamId: string
    batchSeq: number
    projectionVersion: number
    operations: Array<Record<string, unknown>>
  }
  assert.ok(batch)
  assert.equal(batch.batchSeq, 1)
  assert.equal(batch.projectionVersion, 3)
  const kinds = batch.operations.map((operation) => operation.kind)
  assert.deepEqual(kinds, ['snapshot.begin', 'snapshot.items', 'snapshot.commit', 'notifications'])
  const begin = batch.operations[0]
  assert.equal(begin?.sessionId, PLATFORM_ID)
  assert.equal((begin?.meta as Record<string, unknown>).externalSessionId, NATIVE_ID)
  const items = batch.operations[1]?.items as Array<Record<string, unknown>>
  assert.equal(items.length, 1)
  assert.equal(batch.operations[2]?.totalItems, 1)

  assert.deepEqual(await router.request('runtime.sync.ack', { streamId: result.streamId, batchSeq: 1 }, signal()), { ok: true })
  await assert.rejects(
    () => router.request('runtime.sync.ack', { streamId: 'sync_other', batchSeq: 1 }, signal()),
    (error: unknown) => error instanceof BridgeError && error.code === 'INVALID_PARAMS',
  )
  router.close()
})

test('a live event reaches the stream as a timeline upsert', async () => {
  const { router, pushes } = harness()
  await router.request('runtime.sync.subscribe', {}, signal())
  await settle()
  router.onHostChange({
    type: 'event',
    id: NATIVE_ID,
    event: {
      type: 'assistant/message',
      seq: 3,
      time: '2026-01-01T00:00:03Z',
      data: { message: { id: 'm2', content: [{ type: 'text', text: 'second' }] } },
    },
  })
  const result = await router.request('runtime.sync.ack', { streamId: (pushes[0]?.params as { streamId: string }).streamId, batchSeq: 1 }, signal())
  assert.deepEqual(result, { ok: true })
  await settle()
  const second = pushes.filter((entry) => entry.method === 'runtime.sync.batch')[1]?.params as {
    batchSeq: number
    operations: Array<{ kind: string; notifications?: Array<{ method: string; params: Record<string, unknown> }> }>
  }
  assert.equal(second.batchSeq, 2)
  const notification = second.operations[0]?.notifications?.[0]
  assert.equal(notification?.method, 'timeline.itemUpsert')
  assert.equal(notification?.params.sessionId, PLATFORM_ID)
  router.close()
})
