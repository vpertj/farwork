import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { createContext, runInContext } from 'node:vm'

// The browser half is a plain script that registers itself with DSH's module
// loader, so the test drives exactly that contract: capture the load() call, then
// hand the factory a stub require and a fake slots service. No bundler, no DOM.

const SOURCE = readFileSync(new URL('../../client.js', import.meta.url), 'utf8')

interface LoadSpec {
  id: string
  factory: (require: (name: string) => unknown) => Record<string, unknown>
}

interface VNode {
  type: unknown
  props: Record<string, unknown>
  children: unknown[]
}

function captureLoad(): LoadSpec[] {
  const specs: LoadSpec[] = []
  const sandbox = {
    window: { __ModuleLoader__: { load: (spec: LoadSpec) => specs.push(spec) } },
    console,
    // The panel polls only from its effect; a bare vm context has no timers.
    setInterval: () => 0,
    clearInterval: () => undefined,
  }
  runInContext(SOURCE, createContext(sandbox), { filename: 'client.js' })
  return specs
}

function stubReact() {
  return {
    createElement(type: unknown, props: unknown, ...children: unknown[]): VNode {
      return { type, props: (props ?? {}) as Record<string, unknown>, children }
    },
    Fragment: 'Fragment',
    useState<T>(initial: T): [T, (next: T) => void] {
      return [initial, () => undefined]
    },
    useEffect(): void {
      // The panel only mounts its effect when opened; the entry does not.
    },
  }
}

function loadClient(react: unknown = stubReact()): { module: Record<string, unknown>; required: string[] } {
  const specs = captureLoad()
  assert.equal(specs.length, 1)
  const spec = specs[0]!
  assert.equal(spec.id, '@farwork/dsh-bridge')
  assert.equal(typeof spec.factory, 'function')

  const required: string[] = []
  const module = spec.factory((name: string) => {
    required.push(name)
    if (name === 'react') return react
    throw new Error('unexpected require: ' + name)
  })
  return { module, required }
}

interface Registration {
  name: string
  options: Record<string, unknown>
  component: unknown
}

function fakeSlots(options: { throwOnRegister?: boolean; throwOnInject?: boolean } = {}) {
  const injections: Array<{ name: string; produce: () => unknown }> = []
  const registrations: Registration[] = []
  const slots = {
    inject(name: string, produce: () => unknown): void {
      if (options.throwOnInject) throw new Error('inject refused')
      injections.push({ name, produce })
    },
    register(slotOptions: Record<string, unknown>, component: unknown): () => void {
      if (options.throwOnRegister) throw new Error('register refused')
      registrations.push({ name: String(slotOptions.name), options: slotOptions, component })
      return () => undefined
    },
  }
  return { slots, injections, registrations }
}

function texts(node: unknown): string[] {
  if (typeof node === 'string') return [node]
  if (node === null || node === undefined || typeof node !== 'object') return []
  return ((node as VNode).children ?? []).flatMap(texts)
}

test('the client registers one module under the package id', () => {
  const specs = captureLoad()
  assert.equal(specs.length, 1)
  assert.equal(specs[0]!.id, '@farwork/dsh-bridge')
  assert.equal(typeof specs[0]!.factory, 'function')
})

test('the module body stays silent when no module loader is present', () => {
  const specs: LoadSpec[] = []
  assert.doesNotThrow(() => {
    runInContext(SOURCE, createContext({ console }), { filename: 'client.js' })
  })
  assert.deepEqual(specs, [])
})

test('the factory requires only the baseline react module', () => {
  const { required } = loadClient()
  assert.deepEqual(required, ['react'])
})

test('the module exports inject and apply', () => {
  const { module } = loadClient()
  // Both values were created inside the vm realm, so compare contents, not identity.
  assert.deepEqual(Array.from(module.inject as string[]), ['slots'])
  assert.equal(typeof module.apply, 'function')
})

test('apply registers the sidebar footer entry with the documented options', () => {
  const { module } = loadClient()
  const apply = module.apply as (ctx: unknown) => void
  const { slots, injections, registrations } = fakeSlots()

  apply({ slots })

  assert.equal(injections.length, 1)
  assert.equal(injections[0]!.name, 'sidebar.footer.action')
  const produced = injections[0]!.produce()
  assert.equal(typeof produced, 'function')

  assert.equal(registrations.length, 1)
  const registration = registrations[0]!
  assert.equal(registration.name, 'sidebar.footer.action')
  assert.equal(registration.options.name, 'sidebar.footer.action')
  assert.equal(registration.options.id, 'farwork')
  assert.equal(registration.options.order, 26)
  const label = registration.options.label as () => unknown
  assert.equal(typeof label, 'function')
  assert.equal(label(), 'Farwork')
  assert.equal(typeof registration.component, 'function')
})

test('the entry shows its label when wide and only the mark when narrow', () => {
  const { module } = loadClient()
  const { slots, injections, registrations } = fakeSlots()
  ;(module.apply as (ctx: unknown) => void)({ slots })
  injections[0]!.produce()

  const entry = registrations[0]!.component as (props: unknown) => VNode

  const wide = entry({ wide: true })
  assert.deepEqual(texts(wide), ['Farwork'])
  assert.equal(wide.props['aria-label'], 'Farwork')

  // A collapsed sidebar: the label would clip mid-word, so only the mark remains
  // and the accessible name stays.
  const narrow = entry({ wide: false })
  assert.deepEqual(texts(narrow), [])
  assert.equal(narrow.props['aria-label'], 'Farwork')
  assert.equal(narrow.props.title, 'Farwork')
})

/**
 * A renderer that keeps hook state across calls, so the panel's effect can run,
 * settle, and be re-rendered: the error line only exists after that round trip.
 */
function panelRenderer() {
  const states = new Map<unknown, unknown[]>()
  const setters: Array<(value: unknown) => void> = []
  const effects: Array<() => void> = []
  let owner: unknown = null
  let index = 0

  const react = {
    createElement(type: unknown, props: unknown, ...children: unknown[]): VNode {
      return { type, props: (props ?? {}) as Record<string, unknown>, children }
    },
    Fragment: 'Fragment',
    useState<T>(initial: T): [T, (next: T) => void] {
      let slots = states.get(owner)
      if (!slots) {
        slots = []
        states.set(owner, slots)
      }
      const at = index
      index += 1
      if (at >= slots.length) slots[at] = initial
      const set = (next: T): void => {
        slots[at] = next
      }
      setters.push(set as (value: unknown) => void)
      return [slots[at] as T, set]
    },
    useEffect(effect: () => void): void {
      effects.push(effect)
    },
  }

  return {
    react,
    setters,
    effects,
    render(component: (props: unknown) => VNode, props: unknown): VNode {
      owner = component
      index = 0
      return component(props)
    },
  }
}

function isVNode(value: unknown): value is VNode {
  return typeof value === 'object' && value !== null && 'type' in value && 'props' in value
}

/** Open the sidebar entry, mount the panel, and return the text it rendered. */
async function openPanelText(ctx: Record<string, unknown>): Promise<string[]> {
  const renderer = panelRenderer()
  const { module } = loadClient(renderer.react)
  const { slots, injections, registrations } = fakeSlots()
  ;(module.apply as (ctx: unknown) => void)({ ...ctx, slots })
  injections[0]!.produce()

  const entry = registrations[0]!.component as (props: unknown) => VNode
  renderer.render(entry, { wide: true })
  // The entry starts collapsed; opening it is what mounts the panel. No portal
  // target exists here, so react-dom is never required and the card renders inline.
  renderer.setters[0]!(true)
  const opened = renderer.render(entry, { wide: true })
  const panel = opened.children.find((child) => isVNode(child) && typeof child.type === 'function')
  assert.ok(isVNode(panel), 'opening the entry must render the panel')

  const Panel = panel.type as (props: unknown) => VNode
  renderer.render(Panel, panel.props)
  for (const effect of renderer.effects.splice(0)) effect()
  await new Promise((resolve) => setTimeout(resolve, 20))

  return texts(renderer.render(Panel, panel.props))
}

test('a browser without a connection service renders its reason, never an empty panel', async () => {
  const rendered = await openPanelText({})
  assert.ok(rendered.length > 0, 'the panel must not render blank')
  assert.ok(
    rendered.includes('连接服务不可用，请刷新页面'),
    'expected the connection-service line, got: ' + JSON.stringify(rendered),
  )
})

test('a missing panel channel is described as a host-side registration failure', async () => {
  const rendered = await openPanelText({
    connection: {
      rpc: {
        call(): Promise<unknown> {
          return Promise.reject(new Error('no channel registered: /farwork'))
        },
      },
    },
  })
  const line = rendered.find((text) => text.includes('面板通道未注册')) ?? ''
  assert.match(line, /面板通道未注册/)
  assert.match(line, /farwork-dsh-bridge/)
})

test('an HTTP 405 on the channel is reported as a host-side registration failure', async () => {
  // Exactly what the panel showed before the host half registered the channel:
  // the request reached DSH's web server and no route claimed /farwork.
  const rendered = await openPanelText({
    connection: {
      rpc: {
        call(): Promise<unknown> {
          return Promise.reject(new Error('transport failure for /farwork/status: HTTP 405'))
        },
      },
    },
  })
  const line = rendered.find((text) => text.includes('面板通道未注册')) ?? ''
  assert.match(line, /面板通道未注册/)
  assert.match(line, /farwork-dsh-bridge/)
})

test('apply is inert when the slots service is missing', () => {
  const { module } = loadClient()
  const apply = module.apply as (ctx: unknown) => void
  assert.doesNotThrow(() => {
    apply(undefined)
    apply({})
    apply({ slots: null })
    apply({ get: () => undefined })
  })
})

test('apply swallows a slots service that throws', () => {
  const { module } = loadClient()
  const apply = module.apply as (ctx: unknown) => void
  const refusing = fakeSlots({ throwOnRegister: true })
  assert.doesNotThrow(() => {
    apply({ slots: refusing.slots })
    refusing.injections[0]!.produce()
  })
  assert.equal(refusing.registrations.length, 0)

  const noInject = fakeSlots({ throwOnInject: true })
  assert.doesNotThrow(() => apply({ slots: noInject.slots }))
})
