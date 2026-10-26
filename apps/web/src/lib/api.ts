import { asString, isRecord, parseDevices, type DeviceSummary, type HealthResponse } from './protocol';

export const TOKEN_STORAGE_KEY = 'farwork_token';

export function apiBase(): string {
	const configured = import.meta.env.PUBLIC_FARWORK_API ?? '';
	return configured.trim().replace(/\/+$/, '');
}

export function apiUrl(path: string): string {
	return `${apiBase()}${path}`;
}

export function getToken(): string | null {
	try {
		const token = window.localStorage.getItem(TOKEN_STORAGE_KEY);
		const trimmed = token?.trim() ?? '';
		return trimmed.length > 0 ? trimmed : null;
	} catch {
		return null;
	}
}

function authHeaders(): Record<string, string> {
	const token = getToken();
	return token ? { Authorization: `Bearer ${token}` } : {};
}

async function requestJson(path: string): Promise<unknown> {
	const response = await fetch(apiUrl(path), {
		headers: { accept: 'application/json', ...authHeaders() },
		cache: 'no-store'
	});
	if (!response.ok) {
		throw new Error(`${path} ${response.status} ${response.statusText}`.trim());
	}
	return (await response.json()) as unknown;
}

export async function fetchHealth(): Promise<HealthResponse> {
	const payload = await requestJson('/api/health');
	if (!isRecord(payload) || payload.status !== 'ok') {
		throw new Error('health 响应不符合契约');
	}
	const version = asString(payload.version);
	if (version === undefined) throw new Error('health 响应缺少 version');
	return { status: 'ok', version };
}

export async function fetchDevices(): Promise<DeviceSummary[]> {
	const payload = await requestJson('/api/devices');
	if (!isRecord(payload)) throw new Error('devices 响应不符合契约');
	return parseDevices(payload.devices);
}
