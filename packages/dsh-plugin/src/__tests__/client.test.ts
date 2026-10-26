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

function captureLoad(windowExtras: Record<string, unknown> = {}): LoadSpec[] {
  const specs: LoadSpec[] = []
  const sandbox = {
    window: Object.assign({ __ModuleLoader__: { load: (spec: LoadSpec) => specs.push(spec) } }, windowExtras),
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
    useRef<T>(initial: T): { current: T } {
      return { current: initial }
    },
    useEffect(): void {
      // The panel only mounts its effect when opened; the entry does not.
    },
  }
}

function loadClient(
  react: unknown = stubReact(),
  windowExtras: Record<string, unknown> = {},
): { module: Record<string, unknown>; required: string[] } {
  const specs = captureLoad(windowExtras)
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
  // Hooks share one index counter, so refs are keyed like state: same order, same
  // slot, across renders.
  const refs: Array<{ current: unknown }> = []
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
    useRef<T>(initial: T): { current: T } {
      const at = index
      index += 1
      // Refs are numbered by the same hook index state uses, so earlier slots have
      // to exist (as empty holes) before this one is addressed.
      while (refs.length <= at) refs.push({ current: undefined })
      const slot = refs[at] as { current: T }
      if (slot.current === undefined) slot.current = initial
      return slot
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

interface PanelHarness {
  Panel: (props: unknown) => VNode
  props: Record<string, unknown>
  /** Re-render the mounted panel, which is how a settled state or a click shows up. */
  render(): VNode
}

/** Open the sidebar entry, mount the panel, and hand back a way to read it again. */
async function openPanel(
  ctx: Record<string, unknown>,
  windowExtras: Record<string, unknown> = {},
): Promise<PanelHarness> {
  const renderer = panelRenderer()
  const { module } = loadClient(renderer.react, windowExtras)
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

  return {
    Panel,
    props: panel.props,
    render(): VNode {
      return renderer.render(Panel, panel.props)
    },
  }
}

async function openPanelText(ctx: Record<string, unknown>): Promise<string[]> {
  return texts((await openPanel(ctx)).render())
}

function collectNodes(node: unknown, predicate: (candidate: VNode) => boolean): VNode[] {
  if (node === null || node === undefined || typeof node !== 'object') return []
  const vnode = node as VNode
  if (!('type' in vnode) || !('props' in vnode)) return []
  const found = predicate(vnode) ? [vnode] : []
  for (const child of vnode.children ?? []) found.push(...collectNodes(child, predicate))
  return found
}

function inputByLabel(root: unknown, label: string): VNode | undefined {
  return collectNodes(root, (node) => node.type === 'input' && node.props['aria-label'] === label)[0]
}

function buttonByText(root: unknown, label: string): VNode | undefined {
  return collectNodes(root, (node) => node.type === 'button' && texts(node).join('') === label)[0]
}

/** A connection service whose rpc.call answers per endpoint from a fixture. */
function rpcHost(values: Record<string, Record<string, unknown>>): Record<string, unknown> {
  return {
    connection: {
      rpc: {
        call(_channel: string, endpoint: string): Promise<unknown> {
          return Promise.resolve({ ok: true, value: values[endpoint] ?? {} })
        },
      },
    },
  }
}

const STATUS_WITH_HUB = {
  ready: true,
  port: 7501,
  endpointPath: '/tmp/farwork-panel-test/endpoint.json',
  pid: 4242,
  lastError: null,
  version: '0.1.0',
  hub: 'https://hub.example.test',
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

test('the panel explains the login flow and keeps the console button disabled until a hub is set', async () => {
  // No connection at all: the panel still has to show the hub field, the three
  // steps, and a button the user cannot press yet.
  const tree = (await openPanel({})).render()
  const rendered = texts(tree)

  assert.ok(rendered.includes('Web 控制台地址（hub）'), 'expected the hub label, got: ' + JSON.stringify(rendered))
  assert.ok(rendered.includes('打开 Web 控制台并登录'))
  assert.ok(rendered.includes('① 在 Web 控制台登录并生成配对码'))
  assert.ok(rendered.includes('② 回到这里粘贴 connectorToken（配对码只用于生成设备）'))
  assert.ok(rendered.includes('③ 复制下面的命令到终端执行'))
  assert.ok(rendered.includes('尚未配置 hub'))
  assert.ok(rendered.includes('先填 hub 地址'))

  const button = buttonByText(tree, '打开 Web 控制台并登录')
  assert.ok(button, 'the console button must render')
  assert.equal(button.props.disabled, true)
  assert.equal(inputByLabel(tree, 'Web 控制台地址（hub）')?.props.value, '')
})

test('a saved hub is echoed into the field and enables the console button', async () => {
  const tree = (await openPanel(rpcHost({ status: STATUS_WITH_HUB }))).render()

  // status carries the saved hub, so reopening the panel shows it without retyping.
  assert.equal(inputByLabel(tree, 'Web 控制台地址（hub）')?.props.value, 'https://hub.example.test')
  assert.ok(texts(tree).includes('当前 hub：https://hub.example.test'))
  assert.equal(buttonByText(tree, '打开 Web 控制台并登录')?.props.disabled, false)
  assert.equal(texts(tree).includes('先填 hub 地址'), false)
})

test('the console button opens <hub>/#/connect in a new tab with noopener', async () => {
  const opened: Array<{ url: string; target: string; features: string }> = []
  const harness = await openPanel(rpcHost({ status: { ...STATUS_WITH_HUB, hub: 'https://hub.example.test/' } }), {
    open(url: string, target: string, features: string): null {
      opened.push({ url, target, features })
      return null
    },
  })

  const button = buttonByText(harness.render(), '打开 Web 控制台并登录')
  assert.ok(button)
  ;(button.props.onClick as () => void)()

  assert.equal(opened.length, 1)
  // The trailing slash on the saved hub must not become a double slash.
  assert.equal(opened[0]!.url, 'https://hub.example.test/#/connect')
  assert.equal(opened[0]!.target, '_blank')
  assert.match(opened[0]!.features, /noopener/)
  assert.ok(texts(harness.render()).some((text) => text.includes('已打开 Web 控制台')))
})

test('a ws hub cannot be opened in a browser and the panel asks for http(s)', async () => {
  const tree = (
    await openPanel(rpcHost({ status: { ...STATUS_WITH_HUB, hub: 'ws://hub.example.test:7300' } }))
  ).render()

  assert.equal(buttonByText(tree, '打开 Web 控制台并登录')?.props.disabled, true)
  assert.ok(texts(tree).includes('先填 http(s) 的 hub 地址'), 'got: ' + JSON.stringify(texts(tree)))
})

test('saving a hub goes through savePair and echoes the accepted value', async () => {
  const calls: Array<{ endpoint: string; payload: unknown }> = []
  const host = {
    connection: {
      rpc: {
        call(_channel: string, endpoint: string, payload: unknown): Promise<unknown> {
          calls.push({ endpoint, payload })
          if (endpoint === 'savePair') {
            return Promise.resolve({ ok: true, value: { accepted: true, hasToken: false, hub: 'https://hub.example.test' } })
          }
          return Promise.resolve({ ok: true, value: endpoint === 'command' ? { command: 'farwork-agent ...' } : {} })
        },
      },
    },
  }
  const harness = await openPanel(host)

  const input = inputByLabel(harness.render(), 'Web 控制台地址（hub）')
  assert.ok(input)
  ;(input.props.onChange as (event: unknown) => void)({ target: { value: 'https://hub.example.test' } })
  const save = buttonByText(harness.render(), '保存 hub')
  assert.ok(save)
  ;(save.props.onClick as () => void)()
  await new Promise((resolve) => setTimeout(resolve, 0))

  const saveCalls = calls.filter((call) => call.endpoint === 'savePair')
  assert.equal(saveCalls.length, 1)
  assert.equal((saveCalls[0]!.payload as { hub?: unknown }).hub, 'https://hub.example.test')

  const tree = harness.render()
  assert.equal(inputByLabel(tree, 'Web 控制台地址（hub）')?.props.value, 'https://hub.example.test')
  assert.ok(texts(tree).some((text) => text.includes('已保存 hub')), 'got: ' + JSON.stringify(texts(tree)))
})

test('a pasted pairing code shows the host reason instead of a new command', async () => {
  // The panel must not turn the host's refusal into a silent no-op: the reason is
  // the only thing telling the user the code was never a token.
  const host = {
    connection: {
      rpc: {
        call(_channel: string, endpoint: string): Promise<unknown> {
          if (endpoint === 'savePair') {
            return Promise.resolve({
              ok: false,
              error: {
                code: 'farwork/pairing_code',
                message: '这是配对码，不是 connectorToken：请在 Web 控制台用它生成设备后再取 connectorToken',
              },
            })
          }
          return Promise.resolve({ ok: true, value: endpoint === 'command' ? { command: 'farwork-agent ...' } : {} })
        },
      },
    },
  }
  const harness = await openPanel(host)

  const input = inputByLabel(harness.render(), 'connectorToken')
  assert.ok(input)
  ;(input.props.onChange as (event: unknown) => void)({ target: { value: 'AB3D-EFGH' } })
  const button = buttonByText(harness.render(), '重新生成命令')
  assert.ok(button)
  ;(button.props.onClick as () => void)()
  await new Promise((resolve) => setTimeout(resolve, 0))

  const rendered = texts(harness.render())
  assert.ok(rendered.some((text) => text.includes('这是配对码')), 'got: ' + JSON.stringify(rendered))
  assert.ok(rendered.some((text) => text.includes('Web 控制台')))
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
