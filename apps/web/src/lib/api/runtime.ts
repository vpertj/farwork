import { ApiError, apiRequest } from './client';
import {
	parseRuntimeCapabilities,
	parseRuntimeCatalog,
	parseRuntimeCommands,
	parseRuntimeSelections,
	parseSteerDelivery,
	type RuntimeCapabilities,
	type RuntimeCatalogItem,
	type RuntimeCommand,
	type RuntimeSelections
} from './types';

const NOT_RUNNING_CODE = 'session_not_running';

/** 目录名就是 URL 段：hub 只认 model / permission，其余回 422。 */
export type RuntimeCatalogKind = 'model' | 'permission';

function sessionPath(sessionId: string, suffix: string): string {
	return `/sessions/${encodeURIComponent(sessionId)}/runtime${suffix}`;
}

export async function fetchRuntimeCapabilities(
	token: string,
	sessionId: string
): Promise<RuntimeCapabilities> {
	const payload = await apiRequest(sessionPath(sessionId, '/capabilities'), { token });
	const parsed = parseRuntimeCapabilities(payload);
	if (!parsed) throw new Error('响应里没有能力字段（capabilitySet），无法判断这个会话支持哪些运行时操作。');
	return parsed;
}

export async function fetchRuntimeCatalog(
	token: string,
	sessionId: string,
	catalog: RuntimeCatalogKind
): Promise<RuntimeCatalogItem[]> {
	const payload = await apiRequest(
		sessionPath(sessionId, `/catalogs/${encodeURIComponent(catalog)}`),
		{ token }
	);
	return parseRuntimeCatalog(payload);
}

export async function fetchRuntimeCommands(
	token: string,
	sessionId: string
): Promise<RuntimeCommand[]> {
	return parseRuntimeCommands(await apiRequest(sessionPath(sessionId, '/commands'), { token }));
}

export async function steerSession(
	token: string,
	sessionId: string,
	content: string
): Promise<boolean> {
	const payload = await apiRequest(sessionPath(sessionId, '/steer'), {
		method: 'POST',
		token,
		body: { content }
	});
	return parseSteerDelivery(payload);
}

export async function patchRuntimeSelections(
	token: string,
	sessionId: string,
	patch: { model?: string; permissionMode?: string }
): Promise<RuntimeSelections> {
	const payload = await apiRequest(sessionPath(sessionId, '/selections'), {
		method: 'PATCH',
		token,
		body: patch
	});
	const selections = parseRuntimeSelections(payload);
	if (!selections) throw new Error('选择项的响应格式无法识别。');
	return selections;
}

export function isSessionNotRunning(error: unknown): boolean {
	if (!(error instanceof ApiError) || error.status !== 409) return false;
	return error.code === NOT_RUNNING_CODE || /session_not_running/i.test(error.message);
}
