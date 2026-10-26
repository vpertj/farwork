import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, readdir, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'

import { PANEL_FILE, panelPath, readPanelHub, writePanelHub } from '../panel-store.js'

function freshDir(): Promise<string> {
  return mkdtemp(join(tmpdir(), 'farwork-panel-store-'))
}

test('panelPath places panel.json inside the endpoint directory', async () => {
  const dir = await freshDir()
  assert.equal(PANEL_FILE, 'panel.json')
  assert.equal(panelPath(dir), join(dir, 'panel.json'))
})

test('writePanelHub creates a 0600 record in a 0700 directory', async () => {
  const dir = await freshDir()
  const file = panelPath(dir)
  await writePanelHub(file, 'https://hub.example.test')

  assert.equal(await readFile(file, 'utf8'), '{"version":1,"hub":"https://hub.example.test"}\n')
  assert.equal((await stat(file)).mode & 0o777, 0o600)
  assert.equal((await stat(dir)).mode & 0o777, 0o700)
  assert.equal(await readPanelHub(file), 'https://hub.example.test')
})

test('a second write replaces the record and leaves no temporary file behind', async () => {
  const dir = await freshDir()
  const file = panelPath(dir)
  await writePanelHub(file, 'https://old.example.test')
  await writePanelHub(file, 'https://new.example.test')

  assert.equal(await readPanelHub(file), 'https://new.example.test')
  assert.deepEqual((await readdir(dir)).sort(), [PANEL_FILE])
})

test('a missing or broken record reads as null instead of throwing', async () => {
  const dir = await freshDir()
  const file = panelPath(dir)
  assert.equal(await readPanelHub(file), null)

  const broken = ['', 'x', '{"version":1}', '{"version":1,"hub":null}', '{"version":1,"hub":7}', 'null', '[]']
  for (const contents of broken) {
    await writeFile(file, contents, 'utf8')
    assert.equal(await readPanelHub(file), null, 'expected null for: ' + contents)
  }

  // A directory sitting at the record's path is also just "not configured".
  const other = await freshDir()
  await mkdir(panelPath(other))
  assert.equal(await readPanelHub(panelPath(other)), null)
})
