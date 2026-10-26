// Logging that can never be the reason the plugin fails to load.
//
// DSH's logger is a Cordis service; a plugin that throws from its own logger
// would take the whole host down with it, so every call here is best-effort.

export interface Logger {
  debug(message: string, details?: unknown): void
  info(message: string, details?: unknown): void
  warn(message: string, details?: unknown): void
  error(message: string, details?: unknown): void
}

const NOOP: Logger = {
  debug: () => undefined,
  info: () => undefined,
  warn: () => undefined,
  error: () => undefined,
}

type Sink = (message: string, details?: unknown) => void

export function createLogger(ctx: unknown, component: string): Logger {
  const sink = resolveSink(ctx, component)
  const write = (level: string): Sink => (message, details) => {
    try {
      if (details === undefined) sink(`[${level}] ${message}`)
      else sink(`[${level}] ${message} ${JSON.stringify(details)}`)
    } catch {
      // A logger that throws is worse than a lost line.
    }
  }
  return { debug: write('debug'), info: write('info'), warn: write('warn'), error: write('error') }
}

export function NO_LOGGER(): Logger {
  return NOOP
}

function resolveSink(ctx: unknown, component: string): Sink {
  try {
    const host = ctx as { logger?: unknown } | undefined
    if (typeof host?.logger === 'function') {
      const logger = (host.logger as (name: string) => unknown)(component)
      if (logger && typeof logger === 'object') {
        const candidate = logger as Record<string, unknown>
        const bound = candidate.info ?? candidate.log
        if (typeof bound === 'function') {
          return (message: string) => {
            ;(bound as (text: string) => void).call(logger, message)
          }
        }
      }
    }
  } catch {
    // Fall through to the console sink.
  }
  return (message: string) => {
    console.log(`farwork-dsh-bridge ${message}`)
  }
}
