import { asRecord, asString } from './types';

const BASE = (import.meta.env.PUBLIC_FARWORK_API ?? '').trim().replace(/\/+$/, '');
export const API_NAMESPACE = `${BASE}/api/v2`;

export class ApiError extends Error {
	readonly status: number;
	readonly code: string | null;

	constructor(status: number, message: string, code: string | null = null) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.code = code;
	}
}

type Query = Record<string, string | number | boolean | undefined>;

export type RequestOptions = {
	method?: 'GET' | 'POST' | 'PATCH' | 'DELETE';
	body?: unknown;
	token?: string | null;
	query?: Query;
	/** 状态类端点必须绕过 HTTP 缓存：轮询拿到的旧响应会让界面停在假状态上。 */
	cache?: RequestCache;
};

export function apiPath(path: string): string {
	const normalized = path.startsWith('/') ? path : `/${path}`;
	return `${API_NAMESPACE}${normalized}`;
}

export function webSocketUrl(path: string, ticket: string): string {
	const url = new URL(apiPath(path), window.location.origin);
	url.searchParams.set('ticket', ticket);
	url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
	return url.toString();
}

export async function apiRequest(path: string, options: RequestOptions = {}): Promise<unknown> {
	const url = new URL(apiPath(path), window.location.origin);
	if (options.query) {
		for (const [key, value] of Object.entries(options.query)) {
			if (value !== undefined) url.searchParams.set(key, String(value));
		}
	}
	const headers: Record<string, string> = { accept: 'application/json' };
	if (options.body !== undefined) headers['content-type'] = 'application/json';
	if (options.token) headers.authorization = `Bearer ${options.token}`;
	const response = await fetch(url.toString(), {
		method: options.method ?? 'GET',
		headers,
		cache: options.cache,
		body: options.body === undefined ? undefined : JSON.stringify(options.body)
	});
	const text = await response.text();
	const payload: unknown = text.length > 0 ? safeJson(text) : null;
	if (!response.ok) {
		throw new ApiError(response.status, errorMessage(payload, response.status), errorCode(payload));
	}
	return payload;
}

function safeJson(text: string): unknown {
	try {
		return JSON.parse(text) as unknown;
	} catch {
		return text;
	}
}

function errorMessage(payload: unknown, status: number): string {
	const record = asRecord(payload);
	if (record) {
		const detail = record.detail;
		if (typeof detail === 'string' && detail.length > 0) return detail;
		const nested = asRecord(detail);
		if (nested) {
			const message = asString(nested.message);
			if (message) return message;
			const code = asString(nested.code);
			if (code) return code;
		}
		const message = asString(record.message);
		if (message) return message;
	}
	if (typeof payload === 'string' && payload.length > 0) return payload.slice(0, 240);
	return `请求失败（HTTP ${status}）`;
}

function errorCode(payload: unknown): string | null {
	const record = asRecord(payload);
	if (!record) return null;
	const nested = asRecord(record.detail);
	const code = asString(nested?.code) || asString(record.code);
	return code.length > 0 ? code : null;
}

const CLIENT_ID_KEY = 'farwork.client.id';

export function clientId(): string {
	try {
		const stored = window.localStorage.getItem(CLIENT_ID_KEY);
		if (stored && stored.length > 0) return stored;
		const created = `web-${crypto.randomUUID()}`;
		window.localStorage.setItem(CLIENT_ID_KEY, created);
		return created;
	} catch {
		return `web-${Math.random().toString(36).slice(2)}`;
	}
}