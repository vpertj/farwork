export const PLATFORMS = ['macos', 'linux', 'windows'] as const;
export type Platform = (typeof PLATFORMS)[number];

export function isPlatform(value: unknown): value is Platform {
	return typeof value === 'string' && (PLATFORMS as readonly string[]).includes(value);
}

export function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null;
}

export function asString(value: unknown): string | undefined {
	return typeof value === 'string' ? value : undefined;
}

export function asNumber(value: unknown): number | undefined {
	return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

export interface HealthResponse {
	status: 'ok';
	version: string;
}

export interface DeviceSummary {
	id: string;
	name: string;
	platform: Platform;
	connected: boolean;
	last_seen_ms: number;
}

export interface DevicesResponse {
	devices: DeviceSummary[];
}

export interface TerminalOpenFrame {
	type: 'terminal.open';
	stream_id: string;
	cols: number;
	rows: number;
}

export interface TerminalOpenedFrame {
	type: 'terminal.opened';
	stream_id: string;
}

export interface TerminalDataFrame {
	type: 'terminal.data';
	stream_id: string;
	data: string;
}

export interface TerminalExitFrame {
	type: 'terminal.exit';
	stream_id: string;
	code: number;
}

export interface TerminalCloseFrame {
	type: 'terminal.close';
	stream_id: string;
}

export interface TerminalErrorFrame {
	type: 'terminal.error';
	stream_id: string;
	message: string;
}

export interface TerminalInputFrame {
	type: 'terminal.input';
	stream_id: string;
	data: string;
}

export interface TerminalResizeFrame {
	type: 'terminal.resize';
	stream_id: string;
	cols: number;
	rows: number;
}

export interface ConnectorHelloFrame {
	type: 'hello';
	device: { id: string; name: string; platform: Platform };
}

export interface ServerWelcomeFrame {
	type: 'welcome';
	server_version: string;
}

export interface ServerPingFrame {
	type: 'ping';
}

export interface ConnectorPongFrame {
	type: 'pong';
}

export interface DevicesWatchFrame {
	type: 'devices.watch';
}

export interface ClientDevicesFrame {
	type: 'devices';
	devices: DeviceSummary[];
}

export interface ClientTerminalOpenFrame {
	type: 'terminal.open';
	device_id: string;
	stream_id: string;
	cols: number;
	rows: number;
}

export interface ClientTerminalCloseFrame {
	type: 'terminal.close';
	stream_id: string;
}

export type ClientMessage =
	| DevicesWatchFrame
	| ClientTerminalOpenFrame
	| TerminalInputFrame
	| TerminalResizeFrame
	| ClientTerminalCloseFrame
	| ConnectorPongFrame;

export type ServerMessage =
	| ClientDevicesFrame
	| TerminalOpenedFrame
	| TerminalErrorFrame
	| TerminalDataFrame
	| TerminalExitFrame
	| ServerPingFrame;

export type ConnectorMessage =
	| ConnectorHelloFrame
	| TerminalOpenedFrame
	| TerminalDataFrame
	| TerminalExitFrame
	| ConnectorPongFrame;

export function parseDevice(value: unknown): DeviceSummary | null {
	if (!isRecord(value)) return null;
	const id = asString(value.id);
	const name = asString(value.name);
	const connected = value.connected;
	const lastSeen = asNumber(value.last_seen_ms);
	if (!id || name === undefined || !isPlatform(value.platform) || typeof connected !== 'boolean' || lastSeen === undefined) {
		return null;
	}
	return { id, name, platform: value.platform, connected, last_seen_ms: lastSeen };
}

export function parseDevices(value: unknown): DeviceSummary[] {
	const list: unknown[] = Array.isArray(value) ? value : [];
	return list.map(parseDevice).filter((device): device is DeviceSummary => device !== null);
}

export function parseServerMessage(raw: string): ServerMessage | null {
	let decoded: unknown;
	try {
		decoded = JSON.parse(raw);
	} catch {
		return null;
	}
	if (!isRecord(decoded)) return null;

	const streamId = asString(decoded.stream_id);

	switch (decoded.type) {
		case 'devices':
			return { type: 'devices', devices: parseDevices(decoded.devices) };
		case 'ping':
			return { type: 'ping' };
		case 'terminal.opened':
			return streamId ? { type: 'terminal.opened', stream_id: streamId } : null;
		case 'terminal.data': {
			const data = asString(decoded.data);
			return streamId && data !== undefined ? { type: 'terminal.data', stream_id: streamId, data } : null;
		}
		case 'terminal.error': {
			const message = asString(decoded.message);
			return streamId && message !== undefined ? { type: 'terminal.error', stream_id: streamId, message } : null;
		}
		case 'terminal.exit':
			return streamId ? { type: 'terminal.exit', stream_id: streamId, code: asNumber(decoded.code) ?? 0 } : null;
		default:
			return null;
	}
}
