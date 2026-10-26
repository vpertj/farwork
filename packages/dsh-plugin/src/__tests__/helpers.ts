// Shared test plumbing: a real loopback client, so framing is exercised over TCP
// rather than through a mock socket that could hide a write-side bug.

import { connect } from 'node:net'
import type { Socket } from 'node:net'

export interface Message {
  jsonrpc?: string
  id?: unknown
  method?: string
  params?: unknown
  result?: unknown
  error?: { code: number; message: string; data?: { code?: string; retryable?: boolean } }
}

export class Client {
  private readonly socket: Socket
  private buffer = ''
  private readonly pending: Message[] = []
  private waiter: ((message: Message) => void) | null = null
  closed = false

  constructor(socket: Socket) {
    this.socket = socket
    socket.setEncoding('utf8')
    socket.on('data', (chunk: string) => {
      this.buffer += chunk
      let index = this.buffer.indexOf('\n')
      while (index >= 0) {
        const line = this.buffer.slice(0, index)
        this.buffer = this.buffer.slice(index + 1)
        index = this.buffer.indexOf('\n')
        if (line.trim().length === 0) continue
        const message = JSON.parse(line) as Message
        if (this.waiter) {
          const waiter = this.waiter
          this.waiter = null
          waiter(message)
        } else {
          this.pending.push(message)
        }
      }
    })
    socket.on('close', () => {
      this.closed = true
    })
    socket.on('error', () => {
      this.closed = true
    })
  }

  send(payload: unknown): void {
    this.socket.write(JSON.stringify(payload) + '\n')
  }

  sendRaw(text: string): void {
    this.socket.write(text)
  }

  next(timeoutMs = 2_000): Promise<Message> {
    const queued = this.pending.shift()
    if (queued) return Promise.resolve(queued)
    return new Promise<Message>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiter = null
        reject(new Error('timed out waiting for a bridge message'))
      }, timeoutMs)
      this.waiter = (message) => {
        clearTimeout(timer)
        resolve(message)
      }
    })
  }

  close(): void {
    this.socket.destroy()
  }
}

export async function dial(port: number): Promise<Client> {
  const socket = connect(port, '127.0.0.1')
  await new Promise<void>((resolve, reject) => {
    socket.once('connect', () => resolve())
    socket.once('error', reject)
  })
  return new Client(socket)
}

export const TOKEN = 'test-token-0123456789'

export function initializeParams(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    authToken: TOKEN,
    protocolVersion: '1.0',
    runtime: 'dsh',
    connectorId: 'conn_1',
    sessionNamespace: 'farwork-conn_1',
    clientInfo: { name: 'farwork-agent', version: '0.0.0' },
    ...overrides,
  }
}
