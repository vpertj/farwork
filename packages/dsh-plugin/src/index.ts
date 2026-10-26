// @farwork/dsh-bridge - the Farwork bridge as a DSH host plugin.
//
// The entry shape is DSH's: `apply` is called with a Cordis context, and the
// value it returns is treated as the plugin's disposer. Two rules are absolute:
//
// 1. apply() never throws. A bridge that cannot start must cost the user the
//    bridge, never the harness - this session is running inside that harness.
// 2. No bare imports of DSH packages. A linked third-party plugin cannot resolve
//    @deepseek-ai/cordis (it lives inside app.asar), so the host surface is used
//    through duck-typed probes in runtime.ts instead.

import { Bridge, resolveConfig } from './bridge.js'
import type { ResolvedConfig } from './bridge.js'
import { createLogger } from './logger.js'
import type { Logger } from './logger.js'
import { installPanel } from './panel.js'

export type { ResolvedConfig } from './bridge.js'

export const name = 'farwork-dsh-bridge'

interface ConfigIssue {
  message: string
  path?: string[]
}

/**
 * Config validator in the standard-schema shape Cordis expects.
 *
 * Cordis calls Config['~standard'].validate(raw) and reads either `issues` or
 * `value`; a plain defaults object here would throw inside the loader before
 * apply() is reached, which is exactly the failure mode rule 1 forbids.
 */
export const Config = {
  '~standard': {
    version: 1 as const,
    vendor: 'farwork',
    validate(value: unknown): { value: ResolvedConfig } | { issues: ConfigIssue[] } {
      const dict = typeof value === 'object' && value !== null ? (value as Record<string, unknown>) : {}
      const endpointDir = dict.endpointDir
      if (endpointDir !== undefined && typeof endpointDir !== 'string') {
        return { issues: [{ message: 'endpointDir must be a string', path: ['endpointDir'] }] }
      }
      const resolved = resolveConfig(value)
      if (!resolved.endpointDir.startsWith('/')) {
        return { issues: [{ message: 'endpointDir must be an absolute path', path: ['endpointDir'] }] }
      }
      return { value: resolved }
    },
  },
}

export function apply(ctx: unknown, config: unknown): () => void {
  const logger: Logger = safeLogger(ctx)
  try {
    const bridge = new Bridge({ ctx, config: resolveConfig(config), logger })
    // Registered before start(): the panel must be readable while the bridge is
    // still failing, which is exactly when its lastError is the interesting part.
    const panel = installPanel({ ctx, bridge, logger })
    let stopped = false
    const dispose = (): void => {
      if (stopped) return
      stopped = true
      // Released first: the channel is owned by the connection service's context,
      // so it outlives this plugin unless we drop it here.
      panel.dispose()
      void bridge.stop().catch((error: unknown) => {
        logger.warn('the bridge could not shut down cleanly', error)
      })
    }
    // start() resolves the socket and the endpoint file asynchronously; a failure
    // there is reported and swallowed, so the host keeps running without a bridge.
    void bridge.start().catch((error: unknown) => {
      logger.warn('the bridge did not start; DSH continues without it', error)
    })
    return dispose
  } catch (error) {
    // Even constructing the bridge must not be fatal.
    logger.warn('the bridge could not be constructed', error)
    return () => undefined
  }
}

function safeLogger(ctx: unknown): Logger {
  try {
    return createLogger(ctx, 'farwork-dsh-bridge')
  } catch {
    return createLogger(undefined, 'farwork-dsh-bridge')
  }
}
