// Session identity, derived the same way the DSH bridge derives it.
//
// Two identities exist per session and they are not interchangeable:
// * the platform id is what `crates/farwork-agent` sends as `sessionId`, and it is
//   what every sync operation must carry in its `sessionId` field;
// * the native id is DSH's own session id, reported as `externalSessionId`.
// Deriving both deterministically is what makes a retried `createAndStart` land
// on the session the first attempt created instead of a second one.

import { createHash } from 'node:crypto'

import { BridgeError } from './protocol.js'

export function digest(value: string): string {
  return createHash('sha256').update(value, 'utf8').digest('hex')
}

/** DSH session ids: word characters or dashes, at most 128 of them. */
export const PLATFORM_ID = /^[\w-]{1,128}$/

export function namespacePrefix(namespace: string): string {
  return `aa_${digest(namespace).slice(0, 16)}_`
}

/** Platform id → DSH's native session id. */
export function nativeSessionId(namespace: string, platformId: string): string {
  if (!PLATFORM_ID.test(platformId)) {
    throw new BridgeError('INVALID_PARAMS', 'Invalid platform session identity.')
  }
  return `${namespacePrefix(namespace)}${platformId}`
}

/** DSH's native session id → the platform id the wire uses. */
export function sessionId(namespace: string, externalId: string): string {
  const prefix = namespacePrefix(namespace)
  if (externalId.startsWith(prefix)) {
    const rest = externalId.slice(prefix.length)
    if (PLATFORM_ID.test(rest)) return rest
  }
  return `sess_dsh_${digest(`${namespace}:dsh:${externalId}`).slice(0, 24)}`
}

/** Stable timeline item id for one (session, kind, business id) triple. */
export function itemId(externalId: string, kind: string, businessId: string): string {
  return `dsh_${digest(`${externalId}\0${kind}\0${businessId}`)}`
}
