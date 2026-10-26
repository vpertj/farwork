import assert from 'node:assert/strict'
import { test } from 'node:test'

import { createLogger } from '../logger.js'

// The bridge's only report channel is this logger. A refusal that reaches the
// log as `{}` is how "the panel is inert" stayed unexplained for a restart.

function capture(): { ctx: unknown; lines: string[] } {
  const lines: string[] = []
  return {
    ctx: {
      logger: (component: string) => ({
        info(text: string): void {
          lines.push(component + '|' + text)
        },
      }),
    },
    lines,
  }
}

test('an Error passed as details keeps its message and stack instead of {}', () => {
  const { ctx, lines } = capture()
  const logger = createLogger(ctx, 'farwork-dsh-bridge')

  const error = new TypeError("Cannot read properties of undefined (reading 'register')")
  logger.warn('the channel could not be registered', error)

  assert.equal(lines.length, 1)
  assert.match(lines[0]!, /\[warn\] the channel could not be registered/)
  assert.match(lines[0]!, /Cannot read properties of undefined/)
  assert.match(lines[0]!, /TypeError/)
  assert.match(lines[0]!, /"stack"/)
})

test('plain and unprintable details both survive, and a throwing sink stays contained', () => {
  const { ctx, lines } = capture()
  const logger = createLogger(ctx, 'x')

  logger.info('port', { port: 7501 })
  assert.match(lines[0]!, /\{"port":7501\}/)

  const circular: Record<string, unknown> = {}
  circular.self = circular
  assert.doesNotThrow(() => logger.warn('circular', circular))
  assert.equal(lines.length, 2)

  const throwing = createLogger(
    {
      logger: () => ({
        info(): void {
          throw new Error('sink down')
        },
      }),
    },
    'x',
  )
  assert.doesNotThrow(() => throwing.info('still a no-op'))
})
