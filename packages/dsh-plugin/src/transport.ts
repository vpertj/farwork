// Loopback TCP, one JSON document per line, JSON-RPC 2.0.
//
// Three rules this module exists to enforce, each of them a way a bridge can hang
// or be driven by a hostile local peer:
// * a frame is one UTF-8 line and 8 MiB is the ceiling - a peer must never be able
//   to grow the host process by writing without a terminator;
// * a connection is anonymous until initialize proves it holds the endpoint token,
//   and an unauthenticated peer gets no other answer than a refusal;
// * a request has a deadline and a slot: without both, sixteen stalled calls (or
//   one abandoned socket) would freeze every other client.

import { createHash, timingSafeEqual } from 'node:crypto'
import { createServer } from 'node:net'
import type { Server, Socket } from 'node:net'

import { NO_LOGGER } from './logger.js'
import type { Logger } from './logger.js'
import {
  AUTH_TIMEOUT_MS,
  BRIDGE_VERSION,
  BridgeError,
  MAX_CONNECTIONS,
  MAX_FRAME_BYTES,
  MAX_IN_FLIGHT,
  MAX_WRITE_BUFFER_BYTES,
  PROJECTION_VERSION,
  PROTOCOL_VERSION,
  REQUEST_TIMEOUT_MS,
  RUNTIME,
  toErrorPayload,
} from './protocol.js'
import { asSafeInteger, asString, isRecord, nonEmptyString, record } from './values.js'

export interface AuthContext {
  connectorId: string | null
  sessionNamespace: string
  protocolVersion: string
  clientName: string | null
  clientVersion: string | null
}

export interface Router {
  request(method: string, params: unknown, signal: AbortSignal): Promise<unknown>
  /** Called once, after a connection authenticated, with the negotiated identity. */
  initialize?(context: AuthContext): void
}

export type FrameRejectionReason = 'FRAME_TOO_LARGE' | 'NOT_UTF8'

export interface FrameRejection {
  reason: FrameRejectionReason
  bytes: number
}

export interface FrameBatch {
  frames: string[]
  rejected: FrameRejection[]
}

const DECODER = new TextDecoder('utf-8', { fatal: true })

/** Newline framing with a hard ceiling, in both directions. */
export class FrameReader {
  private buffer: Buffer = Buffer.alloc(0)
  private discarding = false

  feed(chunk: Buffer): FrameBatch {
    const frames: string[] = []
    const rejected: FrameRejection[] = []
    this.buffer = this.buffer.length > 0 ? Buffer.concat([this.buffer, chunk]) : chunk

    if (this.discarding) {
      const terminator = this.buffer.indexOf(0x0a)
      if (terminator < 0) {
        this.buffer = Buffer.alloc(0)
        return { frames, rejected }
      }
      this.buffer = this.buffer.subarray(terminator + 1)
      this.discarding = false
    }

    let terminator = this.buffer.indexOf(0x0a)
    while (terminator >= 0) {
      let line = this.buffer.subarray(0, terminator)
      this.buffer = this.buffer.subarray(terminator + 1)
      if (line.length > 0 && line[line.length - 1] === 0x0d) line = line.subarray(0, line.length - 1)
      if (line.length > 0) {
        if (line.length > MAX_FRAME_BYTES) {
          rejected.push({ reason: 'FRAME_TOO_LARGE', bytes: line.length })
        } else {
          try {
            frames.push(DECODER.decode(line))
          } catch {
            // Lossy decoding would turn a broken frame into a plausible request.
            rejected.push({ reason: 'NOT_UTF8', bytes: line.length })
          }
        }
      }
      terminator = this.buffer.indexOf(0x0a)
    }

    if (this.buffer.length > MAX_FRAME_BYTES) {
      rejected.push({ reason: 'FRAME_TOO_LARGE', bytes: this.buffer.length })
      this.buffer = Buffer.alloc(0)
      this.discarding = true
    }
    return { frames, rejected }
  }
}

/** Constant-time token comparison; hashing first keeps the length out of the leak. */
export function tokensMatch(expected: string, supplied: string): boolean {
  const left = createHash('sha256').update(expected, 'utf8').digest()
  const right = createHash('sha256').update(supplied, 'utf8').digest()
  return timingSafeEqual(left, right) && expected.length > 0
}

export interface TransportOptions {
  token: string
  router: Router
  logger?: Logger
  port?: number
  host?: string
  maxConnections?: number
  inFlightMax?: number
  authTimeoutMs?: number
  requestTimeoutMs?: number
  /**
   * Whether an event stream can actually be fed. When the host has no event bus
   * this is false and the handshake says so, so the client refuses up front
   * instead of rendering a timeline that never updates.
   */
  events?: boolean
}

interface Connection {
  socket: Socket
  reader: FrameReader
  authenticated: boolean
  authTimer: NodeJS.Timeout | null
  inFlight: Map<string, AbortController>
}

export class RuntimeServer {
  private readonly token: string
  private readonly router: Router
  private readonly logger: Logger
  private readonly host: string
  private readonly requestedPort: number
  private readonly maxConnections: number
  private readonly inFlightMax: number
  private readonly authTimeoutMs: number
  private readonly requestTimeoutMs: number
  private readonly events: boolean
  private readonly connections = new Set<Connection>()
  private server: Server | null = null
  private boundPort = 0

  constructor(options: TransportOptions) {
    this.token = options.token
    this.router = options.router
    this.logger = options.logger ?? NO_LOGGER()
    this.host = options.host ?? '127.0.0.1'
    this.requestedPort = options.port ?? 0
    this.maxConnections = options.maxConnections ?? MAX_CONNECTIONS
    this.inFlightMax = options.inFlightMax ?? MAX_IN_FLIGHT
    this.authTimeoutMs = options.authTimeoutMs ?? AUTH_TIMEOUT_MS
    this.requestTimeoutMs = options.requestTimeoutMs ?? REQUEST_TIMEOUT_MS
    this.events = options.events ?? true
  }

  get port(): number {
    return this.boundPort
  }

  async listen(): Promise<number> {
    const server = createServer((socket) => this.accept(socket))
    this.server = server
    await new Promise<void>((resolve, reject) => {
      const onError = (error: Error): void => {
        server.removeListener('listening', onListening)
        reject(error)
      }
      const onListening = (): void => {
        server.removeListener('error', onError)
        resolve()
      }
      server.once('error', onError)
      server.once('listening', onListening)
      // Port 0: the OS picks a free loopback port. A fixed port would collide
      // with the upstream bridge and with a second DSH profile.
      server.listen(this.requestedPort, this.host)
    })
    const address = server.address()
    this.boundPort = typeof address === 'object' && address !== null ? address.port : 0
    return this.boundPort
  }

  /** Push a server-initiated notification to every authenticated connection. */
  notify(method: string, params: unknown): void {
    for (const connection of this.connections) {
      if (!connection.authenticated) continue
      this.write(connection, { jsonrpc: '2.0', method, params })
    }
  }

  async close(): Promise<void> {
    for (const connection of this.connections) {
      if (connection.authTimer) clearTimeout(connection.authTimer)
      for (const controller of connection.inFlight.values()) controller.abort('bridge closing')
      connection.socket.destroy()
    }
    this.connections.clear()
    const server = this.server
    this.server = null
    if (!server) return
    await new Promise<void>((resolve) => server.close(() => resolve()))
  }

  private accept(socket: Socket): void {
    if (this.connections.size >= this.maxConnections) {
      this.writeRaw(socket, {
        jsonrpc: '2.0',
        id: null,
        error: { code: -32002, message: 'too many connections', data: { code: 'TOO_MANY_CONNECTIONS', retryable: true } },
      })
      socket.end()
      return
    }
    socket.setNoDelay(true)
    const connection: Connection = {
      socket,
      reader: new FrameReader(),
      authenticated: false,
      authTimer: null,
      inFlight: new Map(),
    }
    // Without this deadline an idle local socket would sit authenticated-less
    // forever, holding one of the sixteen slots.
    const authTimer = setTimeout(() => {
      if (!connection.authenticated) {
        this.logger.warn('connection did not initialize in time; closing it')
        socket.end()
      }
    }, this.authTimeoutMs)
    authTimer.unref?.()
    connection.authTimer = authTimer
    this.connections.add(connection)
    socket.on('data', (chunk: Buffer) => this.onData(connection, chunk))
    socket.on('error', () => socket.destroy())
    socket.on('close', () => {
      if (connection.authTimer) clearTimeout(connection.authTimer)
      connection.authTimer = null
      for (const controller of connection.inFlight.values()) controller.abort('connection closed')
      connection.inFlight.clear()
      this.connections.delete(connection)
    })
  }

  private onData(connection: Connection, chunk: Buffer): void {
    const batch = connection.reader.feed(chunk)
    for (const rejection of batch.rejected) {
      this.logger.warn('rejected a frame', rejection)
      const tooLarge = rejection.reason === 'FRAME_TOO_LARGE'
      this.write(connection, {
        jsonrpc: '2.0',
        id: null,
        error: {
          code: tooLarge ? -32012 : -32700,
          message: tooLarge ? 'frame too large' : 'frame is not UTF-8',
          data: { code: rejection.reason, retryable: false },
        },
      })
      if (!tooLarge) connection.socket.end()
    }
    for (const frame of batch.frames) void this.onFrame(connection, frame)
  }

  private async onFrame(connection: Connection, frame: string): Promise<void> {
    let parsed: unknown
    try {
      parsed = JSON.parse(frame)
    } catch {
      this.write(connection, {
        jsonrpc: '2.0',
        id: null,
        error: { code: -32700, message: 'frame is not JSON', data: { code: 'PARSE_ERROR', retryable: false } },
      })
      return
    }
    if (!isRecord(parsed)) {
      this.write(connection, {
        jsonrpc: '2.0',
        id: null,
        error: { code: -32600, message: 'frame is not a JSON-RPC message', data: { code: 'INVALID_REQUEST', retryable: false } },
      })
      return
    }
    const method = asString(parsed.method)
    if (method === undefined) {
      // A response to a request this bridge never sends; nothing to answer.
      return
    }
    const hasId = Object.hasOwn(parsed, 'id') && parsed.id !== null && parsed.id !== undefined
    if (!hasId) {
      this.onNotification(connection, method, parsed.params)
      return
    }
    await this.onRequest(connection, method, parsed.id, parsed.params)
  }

  private onNotification(connection: Connection, method: string, params: unknown): void {
    if (method !== '$/cancelRequest') {
      this.logger.debug('ignoring a notification', { method })
      return
    }
    const key = requestKey(record(params).id)
    if (!connection.authenticated || key === undefined) return
    const controller = connection.inFlight.get(key)
    if (!controller) return
    controller.abort('cancelled by the peer')
  }

  private async onRequest(connection: Connection, method: string, id: unknown, params: unknown): Promise<void> {
    const key = requestKey(id)
    if (key === undefined) {
      this.write(connection, {
        jsonrpc: '2.0',
        id: id ?? null,
        error: {
          code: -32600,
          message: 'request id must be a string or an integer',
          data: { code: 'INVALID_REQUEST', retryable: false },
        },
      })
      return
    }

    if (method === 'initialize') {
      if (connection.authenticated) {
        this.write(connection, {
          jsonrpc: '2.0',
          id,
          error: toErrorPayload(new BridgeError('INVALID_PARAMS', 'this connection is already initialized')),
        })
        return
      }
      await this.onInitialize(connection, id, params)
      return
    }

    if (!connection.authenticated) {
      // Nothing but initialize is answered on an anonymous connection: a peer
      // without the token must not even learn which methods exist.
      this.write(connection, {
        jsonrpc: '2.0',
        id,
        error: toErrorPayload(new BridgeError('UNAUTHORIZED', 'initialize with a valid authToken first')),
      })
      connection.socket.end()
      return
    }

    if (connection.inFlight.has(key)) {
      this.write(connection, {
        jsonrpc: '2.0',
        id,
        error: toErrorPayload(new BridgeError('INVALID_PARAMS', 'request id ' + key + ' is already in flight')),
      })
      return
    }
    if (connection.inFlight.size >= this.inFlightMax) {
      this.write(connection, {
        jsonrpc: '2.0',
        id,
        error: {
          code: -32002,
          message: 'more than ' + this.inFlightMax + ' requests are already in flight',
          data: { code: 'TOO_MANY_REQUESTS', retryable: true },
        },
      })
      return
    }

    const controller = new AbortController()
    connection.inFlight.set(key, controller)
    const timer = setTimeout(() => controller.abort('request timed out'), this.requestTimeoutMs)
    timer.unref?.()
    try {
      const result = await this.router.request(method, params, controller.signal)
      this.write(connection, { jsonrpc: '2.0', id, result })
    } catch (error) {
      if (controller.signal.aborted) {
        const timedOut = controller.signal.reason === 'request timed out'
        this.write(connection, {
          jsonrpc: '2.0',
          id,
          error: {
            code: timedOut ? -32012 : -32800,
            message: timedOut ? method + ' timed out' : method + ' was cancelled',
            data: { code: timedOut ? 'REQUEST_TIMEOUT' : 'REQUEST_CANCELLED', retryable: timedOut },
          },
        })
      } else {
        this.write(connection, { jsonrpc: '2.0', id, error: toErrorPayload(error) })
      }
    } finally {
      clearTimeout(timer)
      connection.inFlight.delete(key)
    }
  }

  private async onInitialize(connection: Connection, id: unknown, params: unknown): Promise<void> {
    let context: AuthContext
    try {
      context = authenticate(params, this.token)
    } catch (error) {
      this.write(connection, { jsonrpc: '2.0', id, error: toErrorPayload(error) })
      connection.socket.end()
      return
    }
    connection.authenticated = true
    if (connection.authTimer) {
      clearTimeout(connection.authTimer)
      connection.authTimer = null
    }
    try {
      this.router.initialize?.(context)
    } catch (error) {
      // A router that cannot prepare itself must not leave a half-initialized
      // connection that would answer later calls with a guess.
      this.logger.error('router initialize failed', error)
      this.write(connection, { jsonrpc: '2.0', id, error: toErrorPayload(error) })
      connection.socket.end()
      return
    }
    this.write(connection, { jsonrpc: '2.0', id, result: identityPayload({ events: this.events }) })
  }

  private write(connection: Connection, payload: unknown): void {
    this.writeRaw(connection.socket, payload)
  }

  private writeRaw(socket: Socket, payload: unknown): void {
    if (socket.destroyed) return
    // A peer that stopped reading must not turn into unbounded memory here.
    if (socket.writableLength > MAX_WRITE_BUFFER_BYTES) {
      this.logger.warn('write buffer over the limit; dropping the connection')
      socket.destroy()
      return
    }
    try {
      socket.write(JSON.stringify(payload) + '\n')
    } catch (error) {
      this.logger.warn('write failed', error)
      socket.destroy()
    }
  }
}

function requestKey(id: unknown): string | undefined {
  if (typeof id === 'string') return id
  const integer = asSafeInteger(id)
  return integer === undefined ? undefined : String(integer)
}

export function authenticate(params: unknown, token: string): AuthContext {
  const dict = record(params)
  const supplied = asString(dict.authToken) ?? ''
  if (!tokensMatch(token, supplied)) {
    throw new BridgeError('UNAUTHORIZED', 'the bridge token does not match')
  }
  const protocolVersion = asString(dict.protocolVersion) ?? ''
  if (!/^1\.\d+$/.test(protocolVersion)) {
    throw new BridgeError('UNAUTHORIZED', 'protocolVersion must be 1.x, got ' + JSON.stringify(protocolVersion))
  }
  if (asString(dict.runtime) !== RUNTIME) {
    throw new BridgeError('UNAUTHORIZED', 'this bridge serves the ' + RUNTIME + ' runtime only')
  }
  const namespace = nonEmptyString(dict.sessionNamespace) ?? nonEmptyString(dict.connectorId)
  if (namespace === undefined || namespace.length > 512) {
    throw new BridgeError('UNAUTHORIZED', 'sessionNamespace (or connectorId) must be a 1..512 character string')
  }
  const clientInfo = record(dict.clientInfo)
  return {
    connectorId: nonEmptyString(dict.connectorId) ?? null,
    sessionNamespace: namespace,
    protocolVersion,
    clientName: nonEmptyString(clientInfo.name) ?? null,
    clientVersion: nonEmptyString(clientInfo.version) ?? null,
  }
}

/**
 * What this bridge is, as the Rust client reads it.
 *
 * features.approval and features.userQuestions are false and must stay false:
 * v1 carries no interactions, and a true here makes the browser render buttons
 * that nothing answers (see README, "Why there is no approval path").
 */
export function identityPayload(options: { events: boolean } = { events: true }): unknown {
  return {
    identity: {
      runtime: RUNTIME,
      runtimeVersion: BRIDGE_VERSION,
      bridgeVersion: BRIDGE_VERSION,
      protocolVersion: PROTOCOL_VERSION,
      displayName: 'DeepSeek Harness',
    },
    storage: {
      mode: 'dsh-native',
      sameSessionWriterLimit: 1,
      crossProcessWriterExclusion: false,
    },
    features: {
      attachments: false,
      sessionDiscovery: true,
      timelineSuffixRead: false,
      approval: false,
      userQuestions: false,
      readOnly: false,
      snapshotPagination: true,
      // "polling" is not implemented either: it is the honest way to say that no
      // stream will be pushed, and the client then refuses instead of going stale.
      syncMode: options.events ? 'events' : 'polling',
      projectionVersion: PROJECTION_VERSION,
    },
  }
}
