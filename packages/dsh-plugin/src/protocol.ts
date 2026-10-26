// The wire contract, in one place.
//
// These constants are not tunables: each one is either the DSH bridge contract
// (frame ceiling, in-flight cap, deadline, projection version) or its direct
// consequence. Changing one changes what `crates/farwork-agent` may send.

export const PROTOCOL_VERSION = '1.0'
export const RUNTIME = 'dsh'
export const BRIDGE_VERSION = '0.1.0'
export const DISPLAY_NAME = 'DeepSeek Harness'

/** Timeline projection contract version. Bump only with the item schema. */
export const PROJECTION_VERSION = 3

/** One frame is one UTF-8 line; 8 MiB is the hard ceiling. */
export const MAX_FRAME_BYTES = 8 * 1024 * 1024
/** A single sync batch must stay well below the frame ceiling. */
export const MAX_BATCH_BYTES = 7 * 1024 * 1024
export const MAX_BATCH_ITEMS = 250
/** Concurrent requests per connection. */
export const MAX_IN_FLIGHT = 16
/** Per-request deadline; a call that passes it is cancelled. */
export const REQUEST_TIMEOUT_MS = 60_000
/** A peer must send `initialize` within this window. */
export const AUTH_TIMEOUT_MS = 10_000
/** Accepted loopback connections. */
export const MAX_CONNECTIONS = 16
/** A peer that stops reading must not grow the process; drop it instead. */
export const MAX_WRITE_BUFFER_BYTES = 16 * 1024 * 1024
/** Minimum gap between two batches, so one busy turn cannot starve the socket. */
export const MIN_BATCH_GAP_MS = 34
/** How long a batch may stay unacknowledged before the stream fails. */
export const ACK_TIMEOUT_MS = 60_000

export type ErrorCode =
  | 'METHOD_NOT_FOUND'
  | 'INVALID_PARAMS'
  | 'UNSUPPORTED_OPERATION'
  | 'SESSION_NOT_FOUND'
  | 'DSH_SERVICE_UNAVAILABLE'
  | 'FRAME_TOO_LARGE'
  | 'UNAUTHORIZED'
  | 'TOO_MANY_REQUESTS'
  | 'INTERNAL'

/** A refusal this bridge decided on, as opposed to a bug or a dead socket. */
export class BridgeError extends Error {
  readonly code: ErrorCode
  readonly retryable: boolean

  constructor(code: ErrorCode, message: string, retryable = false) {
    super(message)
    this.name = 'BridgeError'
    this.code = code
    this.retryable = retryable
  }
}

const RPC_CODES: Record<ErrorCode, number> = {
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  UNSUPPORTED_OPERATION: -32004,
  SESSION_NOT_FOUND: -32010,
  DSH_SERVICE_UNAVAILABLE: -32011,
  FRAME_TOO_LARGE: -32012,
  UNAUTHORIZED: -32001,
  TOO_MANY_REQUESTS: -32002,
  INTERNAL: -32603,
}

export function rpcCode(code: ErrorCode): number {
  return RPC_CODES[code] ?? RPC_CODES.INTERNAL
}

export interface ErrorPayload {
  code: number
  message: string
  data: { code: string; retryable: boolean }
}

/** Turn any thrown value into the JSON-RPC error object, without leaking internals. */
export function toErrorPayload(error: unknown): ErrorPayload {
  if (error instanceof BridgeError) {
    return {
      code: rpcCode(error.code),
      message: error.message,
      data: { code: error.code, retryable: error.retryable },
    }
  }
  const message = error instanceof Error ? error.message : String(error)
  return {
    code: RPC_CODES.INTERNAL,
    message,
    data: { code: 'INTERNAL', retryable: false },
  }
}
