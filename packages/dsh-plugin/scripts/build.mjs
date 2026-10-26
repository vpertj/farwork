// Type-stripping build: no bundler, no compiler dependency.
//
// Node 22.18+/24 can erase TypeScript syntax by itself, so the package ships the
// same module graph the sources describe, only without annotations. Deviating
// from that (tsc, esbuild) would add a toolchain to a plugin that DSH loads with
// a bare `import()`, and the plugin must stay loadable from a plain checkout.
import { mkdir, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { stripTypeScriptTypes } from 'node:module'
import { dirname, join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = dirname(dirname(fileURLToPath(import.meta.url)))
const sourceDir = join(root, 'src')
const outputDir = join(root, 'dist')

async function collect(dir) {
  const entries = await readdir(dir, { withFileTypes: true })
  const files = []
  for (const entry of entries) {
    const full = join(dir, entry.name)
    if (entry.isDirectory()) files.push(...await collect(full))
    // The source graph imports `./x.js`; the file on disk is `x.ts`.
    else if (entry.name.endsWith('.ts')) files.push(full)
  }
  return files
}

const files = await collect(sourceDir)
await rm(outputDir, { recursive: true, force: true })
for (const file of files) {
  const source = await readFile(file, 'utf8')
  let code
  try {
    code = stripTypeScriptTypes(source, { mode: 'strip', sourceMap: false })
  } catch (error) {
    console.error(`cannot strip types from ${relative(root, file)}: ${error.message}`)
    process.exit(1)
  }
  const target = join(outputDir, relative(sourceDir, file)).replace(/\.ts$/, '.js')
  await mkdir(dirname(target), { recursive: true })
  await writeFile(target, code)
}
console.log(`built ${files.length} module(s) into dist/`)
