// Wiring: lock, listener, endpoint file - in that order, and undone in reverse.
//
// The order matters. A reader that sees endpoint.json must be able to connect to
// it immediately, and a second process must not be able to take the directory
// between the moment the port is up and the moment the endpoint is published.

import { randomBytes } from 'node:crypto'

import { acquireManagerLock, defaultEndpointDir, endpointPath, publishEndpoint, unpublishEndpoint } from './endpoint.js'
import type { ManagerLease } from './endpoint.js'
import type { Logger } from './logger.js'
import { RuntimeRouter } from './router.js'
import { Host, watchEvents } from './runtime.js'
import type { HostChange } from './runtime.js'
import { RuntimeServer } from './transport.js'
import { errorMessage, isRecord } from './values.js'

export interface ResolvedConfig {
  endpointDir: string
}

export interface BridgeOptions {
  ctx: unknown
  config: ResolvedConfig
  logger: Logger
  pid?: number
}

/**
 * What the panel may know about the bridge.
 *
 * The endpoint token is deliberately absent: it is an equivalent credential for
 * this runtime, and the browser half has no use for it (the agent reads the 0600
 * endpoint file itself).
 */
export interface BridgeStatus {
  ready: boolean
  port: number
  endpointPath: string
  pid: number
  /** Why the last start() failed, verbatim: a panel that only says "not ready" is useless. */
  lastError: string | null
}

export class Bridge {
  private readonly ctx: unknown
  private readonly config: ResolvedConfig
  private readonly logger: Logger
  private readonly pid: number
  private readonly token = randomBytes(32).toString('base64url')
  private server: RuntimeServer | null = null
  private lease: ManagerLease | null = null
  private router: RuntimeRouter | null = null
  private unwatch: (() => void) | null = null
  private endpointPublished = false
  private stopping = false
  private lastError: string | null = null

  constructor(options: BridgeOptions) {
    this.ctx = options.ctx
    this.config = options.config
    this.logger = options.logger
    this.pid = options.pid ?? process.pid
  }

  get endpointFile(): string {
    return endpointPath(this.config.endpointDir)
  }

  get port(): number {
    return this.server?.port ?? 0
  }

  get status(): BridgeStatus {
    return {
      // Ready means the published endpoint, not a live listener: until
      // endpoint.json exists no client can connect, so a port alone would
      // overstate readiness.
      ready: this.endpointPublished && !this.stopping,
      port: this.port,
      endpointPath: this.endpointFile,
      pid: this.pid,
      lastError: this.lastError,
    }
  }

  /** Throws only for the caller to log: DSH must start even when this fails. */
  async start(): Promise<void> {
    this.lastError = null
    try {
      await this.startUnsafe()
    } catch (error) {
      // Recorded before rethrowing: the caller still logs it, but the reason has
      // to survive in memory too, or the only trace of it is a log line.
      this.lastError = errorMessage(error)
      throw error
    }
  }

  private async startUnsafe(): Promise<void> {
    const host = new Host(this.ctx)
    const router = new RuntimeRouter({
      host,
      logger: this.logger,
      notify: (method, params) => this.server?.notify(method, params),
    })
    this.router = router

    const unwatch = watchEvents(this.ctx, (change: HostChange) => router.onHostChange(change))
    if (unwatch === null) {
      this.logger.warn('DSH has no event bus; the bridge cannot feed a sync stream')
    }
    this.unwatch = unwatch

    this.lease = await acquireManagerLock(this.config.endpointDir, {
      token: this.token,
      pid: this.pid,
      onCompromised: () => {
        // Another process now owns this directory. Publishing a second endpoint
        // here would give clients two identities for one bridge.
        this.logger.warn('another process took the bridge directory; stopping this instance')
        void this.stop()
      },
    })

    const server = new RuntimeServer({
      token: this.token,
      router,
      logger: this.logger,
      events: unwatch !== null,
    })
    this.server = server
    const port = await server.listen()

    await publishEndpoint(this.endpointFile, {
      version: 1,
      host: '127.0.0.1',
      port,
      token: this.token,
      pid: this.pid,
    })
    this.endpointPublished = true
    this.logger.info('bridge listening', { port, endpoint: this.endpointFile })
  }

  async stop(): Promise<void> {
    if (this.stopping) return
    this.stopping = true
    try {
      if (this.endpointPublished) {
        await unpublishEndpoint(this.endpointFile, this.token, this.pid)
        this.endpointPublished = false
      }
      this.unwatch?.()
      this.unwatch = null
      this.router?.close()
      this.router = null
      const server = this.server
      this.server = null
      if (server) await server.close()
      const lease = this.lease
      this.lease = null
      if (lease) await lease.close()
    } catch (error) {
      this.logger.warn('bridge shutdown was incomplete', error)
    }
  }
}

export function resolveConfig(value: unknown): ResolvedConfig {
  const dict = isRecord(value) ? value : {}
  const requested = typeof dict.endpointDir === 'string' ? dict.endpointDir.trim() : ''
  return { endpointDir: requested.length > 0 ? requested : defaultEndpointDir() }
}
