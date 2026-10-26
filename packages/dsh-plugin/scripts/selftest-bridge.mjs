// End-to-end self test against a fake Cordis context.
//
// It starts the real bridge (manager lock, loopback listener, endpoint file,
// router) inside a temporary endpoint directory and talks to it over TCP exactly
// like crates/farwork-agent does. Nothing here touches ~/.dsh, ~/.farwork or a
// running DSH: the endpoint directory is a fresh mkdtemp and the DSH services are
// fakes defined below.

import { mkdtemp, readFile, readdir, rm } from 'node:fs/promises'
import { connect } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

import { Bridge } from '../dist/bridge.js'
import { nativeSessionId, sessionId } from '../dist/identity.js'
import { createLogger } from '../dist/logger.js'

const NAMESPACE = 'farwork-selftest'
const PLATFORM_ID = 'sess_selftest_1'
// Derived exactly as the bridge derives it, so the two identities round-trip:
// the client sends the platform id and the bridge maps it to this native id.
const NATIVE_ID = nativeSessionId(NAMESPACE, PLATFORM_ID)

function fakeContext() {
  const events = [
    { type: 'turn/start', seq: 1, time: '2026-01-01T00:00:01Z', data: { turn: 1 } },
    {
      type: 'assistant/message',
      seq: 2,
      time: '2026-01-01T00:00:02Z',
      data: { message: { id: 'm1', content: [{ type: 'text', text: 'pong from a fake host' }] } },
    },
  ]
  const services = {
    sessions: {
      get: (id) => (id === NATIVE_ID ? { header: { id: NATIVE_ID }, snapshotEvents: () => events } : undefined),
    },
    sessionQuery: {
      listSessions: async () => [
        { header: { id: NATIVE_ID, cwd: '/tmp', createdAt: '2026-01-01T00:00:00Z' }, live: true, persisted: true },
      ],
      readSession: async () => ({ session: { header: { id: NATIVE_ID } }, events }),
    },
    agents: { get: (id) => (id === NATIVE_ID ? { status: 'idle', cancel: () => undefined } : undefined) },
    sessionController: {
      resolveAgent: async () => ({ agent: { status: 'idle' } }),
      create: async () => undefined,
      prompt: async () => undefined,
    },
    agentPresets: { list: async () => [{ id: 'ptc' }] },
    workspaceRegistry: { archivedSessionIds: [], create: async () => ({ attachSession: async () => undefined }) },
  }
  return {
    ...services,
    get: (name) => services[name],
    // Enough of the event bus for the bridge to advertise an event stream.
    on: () => () => undefined,
  }
}

class Peer {
  constructor(socket) {
    this.socket = socket
    this.buffer = ''
    this.queue = []
    this.waiters = []
    socket.setEncoding('utf8')
    socket.on('data', (chunk) => {
      this.buffer += chunk
      let index = this.buffer.indexOf('\n')
      while (index >= 0) {
        const line = this.buffer.slice(0, index)
        this.buffer = this.buffer.slice(index + 1)
        index = this.buffer.indexOf('\n')
        if (!line.trim()) continue
        const message = JSON.parse(line)
        const waiter = this.waiters.shift()
        if (waiter) waiter(message)
        else this.queue.push(message)
      }
    })
  }

  send(payload) {
    const text = JSON.stringify(payload)
    transcript.push('--> ' + text)
    this.socket.write(text + '\n')
  }

  async next() {
    const queued = this.queue.shift()
    if (queued) {
      transcript.push('<-- ' + JSON.stringify(queued))
      return queued
    }
    return new Promise((resolve) => {
      this.waiters.push((message) => {
        transcript.push('<-- ' + JSON.stringify(message))
        resolve(message)
      })
    })
  }
}

const transcript = []

async function main() {
  const endpointDir = await mkdtemp(join(tmpdir(), 'farwork-bridge-selftest-'))
  const bridge = new Bridge({ ctx: fakeContext(), config: { endpointDir }, logger: createLogger(undefined, 'selftest') })
  await bridge.start()
  const record = JSON.parse(await readFile(join(endpointDir, 'endpoint.json'), 'utf8'))
  console.log('endpoint dir :', endpointDir)
  console.log('endpoint     :', JSON.stringify({ ...record, token: '<redacted>' }))
  console.log('pid          :', process.pid)

  const socket = connect(record.port, record.host)
  await new Promise((resolve, reject) => {
    socket.once('connect', resolve)
    socket.once('error', reject)
  })
  const peer = new Peer(socket)
  const initialize = {
    jsonrpc: '2.0',
    id: 'aa-1',
    method: 'initialize',
    params: {
      authToken: record.token,
      protocolVersion: '1.0',
      runtime: 'dsh',
      connectorId: 'conn_selftest',
      sessionNamespace: NAMESPACE,
      clientInfo: { name: 'farwork-agent', version: 'selftest' },
    },
  }
  peer.send(initialize)
  const handshake = await peer.next()
  const features = (handshake.result ?? {}).features ?? {}

  peer.send({ jsonrpc: '2.0', id: 'aa-2', method: 'ping', params: {} })
  await peer.next()

  peer.send({ jsonrpc: '2.0', id: 'aa-3', method: 'session.list', params: { limit: 5 } })
  const listed = await peer.next()

  peer.send({ jsonrpc: '2.0', id: 'aa-4', method: 'runtime.sync.subscribe', params: {} })
  const subscribed = await peer.next()
  const streamId = (subscribed.result ?? {}).streamId
  const batch = await peer.next()
  const batchSeq = (batch.params ?? {}).batchSeq
  peer.send({ jsonrpc: '2.0', id: 'aa-5', method: 'runtime.sync.ack', params: { streamId, batchSeq } })
  await peer.next()

  // Negative path: a method outside the v1 set answers -32601 rather than guessing.
  peer.send({ jsonrpc: '2.0', id: 'aa-6', method: 'session.getNotices', params: {} })
  await peer.next()

  console.log('--- transcript ---')
  for (const line of transcript) {
    const shown = line.includes(record.token) ? line.split(record.token).join('<redacted>') : line
    console.log(shown.length > 400 ? shown.slice(0, 400) + ' ...' : shown)
  }
  console.log('--- summary ---')
  console.log('identity.runtime :', handshake.result?.identity?.runtime)
  console.log('features.syncMode:', features.syncMode, '| projectionVersion:', features.projectionVersion)
  console.log('features.approval:', features.approval)
  const first = (listed.result ?? {}).sessions?.[0]
  console.log('sessions listed  :', ((listed.result ?? {}).sessions ?? []).length, '| sessionId:', first?.sessionId, '| externalSessionId:', first?.externalSessionId)
  console.log('dual id roundtrip:', sessionId(NAMESPACE, first?.externalSessionId ?? '') === PLATFORM_ID)
  console.log('stream           :', streamId, '| first batchSeq:', batchSeq, '| operations:', (batch.params?.operations ?? []).length)

  socket.destroy()
  await bridge.stop()
  const stillThere = await readFile(join(endpointDir, 'endpoint.json'), 'utf8').then(() => true, () => false)
  console.log('endpoint removed on stop:', !stillThere)
  const leftover = await readdir(endpointDir)
  console.log('files left in the endpoint dir:', leftover.length === 0 ? '(none)' : leftover.join(', '))
  await rm(endpointDir, { recursive: true, force: true })
}

await main()
