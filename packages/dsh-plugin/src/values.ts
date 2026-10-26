// Narrowing helpers for values that cross the wire from DSH.
//
// Everything from the host is `unknown`: DSH services are not type-checked
// against this plugin, so a shape that changed upstream must degrade into a
// readable error instead of a TypeError deep inside a request handler.

export type Dict = Record<string, unknown>

export function isRecord(value: unknown): value is Dict {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function asString(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined
}

export function nonEmptyString(value: unknown): string | undefined {
  const text = asString(value)
  if (text === undefined) return undefined
  const trimmed = text.trim()
  return trimmed.length > 0 ? text : undefined
}

export function asNumber(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined
}

export function asSafeInteger(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isSafeInteger(value) ? value : undefined
}

export function asBoolean(value: unknown): boolean | undefined {
  return typeof value === 'boolean' ? value : undefined
}

export function asArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : []
}

export function record(value: unknown): Dict {
  return isRecord(value) ? value : {}
}

/** Byte length of the JSON encoding the transport will actually write. */
export function bytesOf(value: unknown): number {
  try {
    return Buffer.byteLength(JSON.stringify(value ?? null), 'utf8')
  } catch {
    return Number.MAX_SAFE_INTEGER
  }
}

export function timestampOf(value: unknown): string {
  const text = asString(value)
  if (text !== undefined) return text
  return new Date().toISOString()
}
