import { apiBase, getToken } from './api';
import { base64ToBytes, textToBase64 } from './base64';
import {
	parseServerMessage,
	type ClientMessage,
	type DeviceSummary,
	type ServerMessage
} from './protocol';

export type SocketState = 'idle' | 'connecting' | 'open' | 'reconnecting' | 'closed';

export interface TerminalStreamHandlers {
	onOpened: () => void;
	onData: (bytes: Uint8Array) => void;
	onError: (message: string) => void;
	onExit: (code: number) => void;
	onDisconnected: () => void;
}

export interface TerminalHandle {
	readonly streamId: string;
	write: (data: string) => void;
	resize: (cols: number, rows: number) => void;
	close: () => void;
}

const MIN_BACKOFF = 500;
const MAX_BACKOFF = 8000;

export function wsUrl(path: string): string {
	const configured = apiBase();
	const origin = configured.length > 0 ? configured : window.location.origin;
	const url = new URL(origin);
	url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
	url.pathname = path;
	url.search = '';
	url.hash = '';
	return url.toString();
}

export class FarworkClient {
	readonly #baseUrl: string;
	readonly #stateHandlers = new Set<(state: SocketState) => void>();
	readonly #devicesHandlers = new Set<(devices: DeviceSummary[]) => void>();
	readonly #streams = new Map<string, TerminalStreamHandlers>();
	#socket: WebSocket | null = null;
	#state: SocketState = 'idle';
	#attempt = 0;
	#timer: ReturnType<typeof setTimeout> | null = null;
	#disposed = false;

	constructor(baseUrl: string) {
		this.#baseUrl = baseUrl;
	}

	get state(): SocketState {
		return this.#state;
	}

	isOpen(): boolean {
		return this.#socket !== null && this.#socket.readyState === WebSocket.OPEN;
	}

	onState(handler: (state: SocketState) => void): () => void {
		this.#stateHandlers.add(handler);
		handler(this.#state);
		return () => this.#stateHandlers.delete(handler);
	}

	onDevices(handler: (devices: DeviceSummary[]) => void): () => void {
		this.#devicesHandlers.add(handler);
		return () => this.#devicesHandlers.delete(handler);
	}

	connect(): void {
		if (this.#disposed || this.#socket !== null) return;
		this.#setState(this.#attempt === 0 ? 'connecting' : 'reconnecting');

		const token = getToken();
		const target = token ? `${this.#baseUrl}?token=${encodeURIComponent(token)}` : this.#baseUrl;
		const socket = new WebSocket(target);
		this.#socket = socket;

		socket.addEventListener('open', () => {
			if (this.#socket !== socket) return;
			this.#attempt = 0;
			this.#setState('open');
			this.#send({ type: 'devices.watch' });
		});

		socket.addEventListener('message', (event) => {
			if (this.#socket !== socket) return;
			const raw: unknown = event.data;
			if (typeof raw !== 'string') return;
			this.#handleMessage(raw);
		});

		socket.addEventListener('close', () => {
			if (this.#socket !== socket) return;
			this.#socket = null;
			this.#failStreams();
			if (this.#disposed) {
				this.#setState('closed');
				return;
			}
			this.#scheduleReconnect();
		});

		socket.addEventListener('error', () => {
			socket.close();
		});
	}

	reconnect(): void {
		this.#clearTimer();
		this.#disposed = false;
		this.#attempt = 0;
		const socket = this.#socket;
		this.#socket = null;
		this.#failStreams();
		if (socket) socket.close();
		this.connect();
	}

	close(): void {
		this.#disposed = true;
		this.#clearTimer();
		this.#failStreams();
		const socket = this.#socket;
		this.#socket = null;
		if (socket) socket.close();
		this.#setState('closed');
	}

	openTerminal(
		deviceId: string,
		cols: number,
		rows: number,
		handlers: TerminalStreamHandlers
	): TerminalHandle {
		const streamId = crypto.randomUUID();
		this.#streams.set(streamId, handlers);
		this.#send({ type: 'terminal.open', device_id: deviceId, stream_id: streamId, cols, rows });

		return {
			streamId,
			write: (data: string) => {
				this.#send({ type: 'terminal.input', stream_id: streamId, data: textToBase64(data) });
			},
			resize: (nextCols: number, nextRows: number) => {
				this.#send({ type: 'terminal.resize', stream_id: streamId, cols: nextCols, rows: nextRows });
			},
			close: () => {
				this.#send({ type: 'terminal.close', stream_id: streamId });
				this.#streams.delete(streamId);
			}
		};
	}

	#handleMessage(raw: string): void {
		const message: ServerMessage | null = parseServerMessage(raw);
		if (!message) return;

		switch (message.type) {
			case 'devices':
				for (const handler of this.#devicesHandlers) handler(message.devices);
				return;
			case 'ping':
				this.#send({ type: 'pong' });
				return;
			case 'terminal.opened':
				this.#streams.get(message.stream_id)?.onOpened();
				return;
			case 'terminal.data':
				this.#streams.get(message.stream_id)?.onData(base64ToBytes(message.data));
				return;
			case 'terminal.error': {
				const stream = this.#streams.get(message.stream_id);
				this.#streams.delete(message.stream_id);
				stream?.onError(message.message);
				return;
			}
			case 'terminal.exit': {
				const stream = this.#streams.get(message.stream_id);
				this.#streams.delete(message.stream_id);
				stream?.onExit(message.code);
				return;
			}
		}
	}

	#send(message: ClientMessage): boolean {
		const socket = this.#socket;
		if (!socket || socket.readyState !== WebSocket.OPEN) return false;
		socket.send(JSON.stringify(message));
		return true;
	}

	#failStreams(): void {
		const streams = [...this.#streams.values()];
		this.#streams.clear();
		for (const stream of streams) stream.onDisconnected();
	}

	#scheduleReconnect(): void {
		this.#clearTimer();
		const delay = Math.min(MIN_BACKOFF * 2 ** this.#attempt, MAX_BACKOFF);
		this.#attempt += 1;
		this.#setState('reconnecting');
		this.#timer = setTimeout(() => {
			this.#timer = null;
			this.connect();
		}, delay);
	}

	#clearTimer(): void {
		if (this.#timer !== null) {
			clearTimeout(this.#timer);
			this.#timer = null;
		}
	}

	#setState(next: SocketState): void {
		if (this.#state === next) return;
		this.#state = next;
		for (const handler of this.#stateHandlers) handler(next);
	}
}
