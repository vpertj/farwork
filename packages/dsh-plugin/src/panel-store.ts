// The one panel fact that outlives the process: the hub address.
//
// The hub is a public URL a user would otherwise retype after every DSH restart,
// and the "open the Web console" button is worthless without it. The connector
// token is absent from this file on purpose: it is a credential for this runtime,
// and a readable copy on disk would only widen the attack surface.
//
// Written with a temporary file + rename, the same shape the endpoint and the
// sync checkpoints use, so a reader never observes a half-written record.

import { chmod, open, readFile, rename, unlink } from 'node:fs/promises'
import { dirname, join } from 'node:path'

import { ensurePrivateDir } from './endpoint.js'

export const PANEL_FILE = 'panel.json'

export interface PanelRecord {
  version: 1
  hub: string
}

export function panelPath(dir: string): string {
  return join(dir, PANEL_FILE)
}

/**
 * The remembered hub, or null when there is none.
 *
 * Every failure - missing file, unreadable file, invalid JSON, a version this
 * build does not know, a hub that is not a non-empty string - reads as "not
 * configured". A corrupt file must never be a reason for the bridge to fail to
 * start: the worst it may cost is one retyped URL.
 */
export async function readPanelHub(file: string): Promise<string | null> {
  try {
    const raw = await readFile(file, 'utf8')
    const parsed: unknown = JSON.parse(raw)
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return null
    const record = parsed as Record<string, unknown>
    if (record.version !== 1) return null
    const hub = record.hub
    if (typeof hub !== 'string') return null
    const trimmed = hub.trim()
    return trimmed.length > 0 ? trimmed : null
  } catch {
    return null
  }
}

/** Persist the hub, replacing any previous record atomically. Throws on failure. */
export async function writePanelHub(file: string, hub: string): Promise<void> {
  const dir = dirname(file)
  await ensurePrivateDir(dir)
  const record: PanelRecord = { version: 1, hub }
  // The name carries the pid so a leftover temp file cannot be mistaken for this
  // process's record, and 'wx' refuses to reuse one at all.
  const temp = `${file}.${process.pid}.tmp`
  await unlink(temp).catch(ignoreMissing)
  const handle = await open(temp, 'wx', 0o600)
  try {
    await handle.writeFile(`${JSON.stringify(record)}\n`, 'utf8')
  } finally {
    await handle.close()
  }
  try {
    await rename(temp, file)
  } catch (error) {
    await unlink(temp).catch(() => undefined)
    throw error as Error
  }
  // open()'s mode is masked by the umask; set the bits explicitly so a widened
  // umask cannot leave the record group- or world-readable.
  await chmod(file, 0o600)
}

function ignoreMissing(error: unknown): void {
  const code = (error as { code?: string } | undefined)?.code
  if (code === 'ENOENT') return
  throw error as Error
}
