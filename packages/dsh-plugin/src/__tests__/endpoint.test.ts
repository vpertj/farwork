import assert from 'node:assert/strict'
import { mkdtemp, readdir, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'

import {
  acquireManagerLock,
  endpointPath,
  lockPath,
  publishEndpoint,
  readEndpoint,
  readLock,
  unpublishEndpoint,
} from '../endpoint.js'

async function tempDir(): Promise<string> {
  return mkdtemp(join(tmpdir(), 'farwork-bridge-'))
}

test('the endpoint is published with 0600 into a 0700 directory and leaves no temp file', async () => {
  const dir = await tempDir()
  const file = endpointPath(dir)
  await publishEndpoint(file, { version: 1, host: '127.0.0.1', port: 1234, token: 'tok', pid: 42 })

  const mode = (await stat(file)).mode & 0o777
  assert.equal(mode, 0o600)
  assert.equal((await stat(dir)).mode & 0o777, 0o700)
  assert.deepEqual(await readEndpoint(file), { version: 1, host: '127.0.0.1', port: 1234, token: 'tok', pid: 42 })
  // The temp file is unlinked after the link() publish. .DS_Store aside, only the
  // endpoint itself may remain.
  const entries = (await readdir(dir)).filter((entry) => entry !== '.DS_Store')
  assert.deepEqual(entries, ['endpoint.json'])
})

test('republishing replaces the record atomically', async () => {
  const dir = await tempDir()
  const file = endpointPath(dir)
  await publishEndpoint(file, { version: 1, host: '127.0.0.1', port: 1, token: 'first', pid: 1 })
  await publishEndpoint(file, { version: 1, host: '127.0.0.1', port: 2, token: 'second', pid: 2 })
  const record = await readEndpoint(file)
  assert.equal(record?.port, 2)
  assert.equal(record?.token, 'second')
})

test('unpublishing requires the exact token and pid', async () => {
  const dir = await tempDir()
  const file = endpointPath(dir)
  await publishEndpoint(file, { version: 1, host: '127.0.0.1', port: 1, token: 'mine', pid: 7 })
  assert.equal(await unpublishEndpoint(file, 'other', 7), false)
  assert.equal(await unpublishEndpoint(file, 'mine', 8), false)
  assert.ok(await readEndpoint(file))
  assert.equal(await unpublishEndpoint(file, 'mine', 7), true)
  assert.equal(await readEndpoint(file), null)
})

test('the lock is a listener, and closing it removes the record', async () => {
  const dir = await tempDir()
  const lease = await acquireManagerLock(dir, { token: 'lock-token', pid: 11 })
  assert.ok(lease.isOwner())
  assert.ok(lease.record.port > 0)
  const raw = await readLock(lockPath(dir))
  assert.equal(raw?.token, 'lock-token')
  assert.equal(raw?.pid, 11)
  assert.equal(raw?.port, lease.record.port)
  await lease.close()
  assert.equal(await readLock(lockPath(dir)), null)
})

test('a second lock holder compromises the first', async () => {
  const dir = await tempDir()
  let compromised = 0
  const first = await acquireManagerLock(dir, {
    token: 'first',
    pid: 21,
    intervalMs: 10,
    onCompromised: () => {
      compromised += 1
    },
  })
  const second = await acquireManagerLock(dir, { token: 'second', pid: 22, intervalMs: 10 })
  await new Promise((resolve) => setTimeout(resolve, 60))
  assert.equal(compromised, 1)
  assert.equal(first.isOwner(), false)
  assert.ok(second.isOwner())
  await second.close()
  await first.close()
})
