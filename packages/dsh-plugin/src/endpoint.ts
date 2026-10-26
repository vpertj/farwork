// The rendezvous file and the per-directory manager lock.
//
// Two invariants, both learned the hard way upstream:
// * the file is published with `link()`, never with a rename or a direct write —
//   a reader must never observe a half-written endpoint, and `link` fails instead
//   of clobbering a competitor's file;
// * the lock lives *next to the endpoint file*, so a bridge in another directory
//   cannot steal it. That is the only reason this bridge can coexist with the
//   upstream one.

import { link, mkdir, readFile, chmod, unlink } from 'node:fs/promises'
import { open } from 'node:fs/promises'
import { createServer } from 'node:net'
import type { Server, Socket } from 'node:net'
import { homedir } from 'node:os'
import { dirname, join } from 'node:path'

export const ENDPOINT_FILE = 'endpoint.json'
export const LOCK_FILE = '.manager-lock.json'
export const LOOPBACK = '127.0.0.1'

export interface EndpointRecord {
  version: 1
  host: string
  port: number
  token: string
  pid: number
}

export interface LockRecord {
  version: 1
  port: number
  pid: number
  token: string
  startedAt: string
}

/**
 * `~/.farwork/dsh-bridge`.
 *
 * Deliberately not `~/.agents-anywhere/dsh-bridge`: that directory is the upstream
 * plugin's rendezvous point, and sharing it would make the two bridges evict each
 * other's endpoint and lock.
 */
export function defaultEndpointDir(env: Record<string, string | undefined> = process.env): string {
  const home = env.HOME ?? env.USERPROFILE ?? homedir()
  return join(home, '.farwork', 'dsh-bridge')
}

export function endpointPath(dir: string): string {
  return join(dir, ENDPOINT_FILE)
}

export function lockPath(dir: string): string {
  return join(dir, LOCK_FILE)
}

export async function ensurePrivateDir(dir: string): Promise<void> {
  await mkdir(dir, { recursive: true, mode: 0o700 })
  // `mode` on mkdir is masked by the umask and ignored for existing dirs.
  await chmod(dir, 0o700)
}

async function writeExclusive(file: string, contents: string): Promise<void> {
  // `wx` refuses to reuse a leftover temp file: a stale one would otherwise hold
  // a token that no longer matches this process.
  await unlink(file).catch(ignoreMissing)
  const handle = await open(file, 'wx', 0o600)
  try {
    await handle.writeFile(contents, 'utf8')
  } finally {
    await handle.close()
  }
}

/** Publish the endpoint atomically; readers see either nothing or a full record. */
export async function publishEndpoint(file: string, record: EndpointRecord): Promise<void> {
  await ensurePrivateDir(dirname(file))
  const temp = `${file}.${record.token}.tmp`
  await writeExclusive(temp, `${JSON.stringify(record)}\n`)
  try {
    await unlink(file).catch(ignoreMissing)
    await link(temp, file)
  } finally {
    await unlink(temp).catch(ignoreMissing)
  }
}

/** Remove the endpoint only if it still describes this exact process. */
export async function unpublishEndpoint(file: string, token: string, pid: number): Promise<boolean> {
  const current = await readEndpoint(file)
  if (!current) return false
  if (current.token !== token || current.pid !== pid) return false
  try {
    await unlink(file)
    return true
  } catch {
    return false
  }
}

export async function readJsonRecord(file: string): Promise<Record<string, unknown> | null> {
  try {
    const raw = await readFile(file, 'utf8')
    const parsed: unknown = JSON.parse(raw)
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return null
    return parsed as Record<string, unknown>
  } catch {
    return null
  }
}

export async function readEndpoint(file: string): Promise<EndpointRecord | null> {
  const record = await readJsonRecord(file)
  if (!record) return null
  if (record.version !== 1) return null
  if (typeof record.host !== 'string' || typeof record.port !== 'number') return null
  if (typeof record.token !== 'string' || typeof record.pid !== 'number') return null
  return {
    version: 1,
    host: record.host,
    port: record.port,
    token: record.token,
    pid: record.pid,
  }
}

/** The lock record, or null when it is missing or belongs to someone else. */
export async function readLock(file: string): Promise<LockRecord | null> {
  const record = await readJsonRecord(file)
  if (!record) return null
  if (record.version !== 1) return null
  if (typeof record.port !== 'number' || typeof record.pid !== 'number') return null
  if (typeof record.token !== 'string') return null
  return {
    version: 1,
    port: record.port,
    pid: record.pid,
    token: record.token,
    startedAt: typeof record.startedAt === 'string' ? record.startedAt : '',
  }
}

export interface ManagerLease {
  readonly record: LockRecord
  /** False once another process took the directory over. */
  isOwner(): boolean
  close(): Promise<void>
}

export interface LeaseOptions {
  token: string
  pid: number
  intervalMs?: number
  onCompromised?: (reason: string) => void
}

/**
 * Take the per-directory manager lock.
 *
 * The lock is a loopback listener, not a pid: an abnormally exited process
 * releases its port automatically, so liveness never has to be guessed from a
 * pid that may have been recycled.
 */
export async function acquireManagerLock(dir: string, options: LeaseOptions): Promise<ManagerLease> {
  await ensurePrivateDir(dir)
  const token = options.token
  const pid = options.pid
  let compromised = false

  const server: Server = createServer((socket: Socket) => {
    // A competitor must prove it read the record: answering `1` to a matching
    // token is the handshake that makes the lock meaningful.
    let buffer = ''
    socket.setEncoding('utf8')
    socket.on('data', (chunk: string) => {
      buffer += chunk
      if (!buffer.includes('\n')) return
      let ok = false
      try {
        const parsed = JSON.parse(buffer.slice(0, buffer.indexOf('\n'))) as { token?: unknown }
        ok = parsed.token === token
      } catch {
        ok = false
      }
      socket.end(ok ? '1\n' : '0\n')
    })
    socket.on('error', () => socket.destroy())
  })

  await new Promise<void>((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, LOOPBACK, () => {
      server.removeListener('error', reject)
      resolve()
    })
  })

  const address = server.address()
  const port = typeof address === 'object' && address !== null ? address.port : 0
  const record: LockRecord = {
    version: 1,
    port,
    pid,
    token,
    startedAt: new Date().toISOString(),
  }
  const file = lockPath(dir)
  await writeExclusive(file, `${JSON.stringify(record)}\n`)

  const interval = options.intervalMs ?? 5_000
  const timer = setInterval(() => {
    void readLock(file).then((lock) => {
      if (compromised) return
      const stillOurs = lock !== null && lock.token === token && lock.pid === pid && lock.port === port
      if (stillOurs) return
      compromised = true
      clearInterval(timer)
      try {
        options.onCompromised?.('superseded')
      } catch {
        // A callback that throws must not take the interval down silently.
      }
    })
  }, interval)
  timer.unref?.()

  return {
    record,
    isOwner: () => !compromised,
    async close() {
      clearInterval(timer)
      // A competitor may still be mid-handshake; closing the listener must not
      // wait for it forever.
      server.closeAllConnections?.()
      await new Promise<void>((resolve) => server.close(() => resolve()))
      const current = await readLock(file)
      if (current && current.token === token && current.pid === pid && current.port === port) {
        await unlink(file).catch(ignoreMissing)
      }
    },
  }
}

function ignoreMissing(error: unknown): void {
  const code = (error as { code?: string } | undefined)?.code
  if (code === 'ENOENT') return
  throw error as Error
}
