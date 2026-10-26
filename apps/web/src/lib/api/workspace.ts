import { webSocketUrl, apiRequest, clientId } from './client';
import {
	asRecord,
	asString,
	cursorToSeq,
	parseConnectors,
	parseCreateAndStart,
	parseNotice,
	parseNotices,
	parseProjects,
	parseRecovery,
	parseSessions,
	parseSnapshot,
	seqToCursor,
	type Connector,
	type CreateAndStartResult,
	type EventRecovery,
	type NoticeDecision,
	type Project,
	type RuntimeNotice,
	type Session,
	type SessionSnapshot
} from './types';

export async function listConnectors(token: string): Promise<Connector[]> {
	return parseConnectors(await apiRequest('/connectors', { token }));
}

export async function listProjects(token: string): Promise<Project[]> {
	return parseProjects(await apiRequest('/projects', { token }));
}

export async function listSessions(token: string): Promise<Session[]> {
	return parseSessions(await apiRequest('/sessions/list', { token }));
}

export async function fetchSnapshot(
	token: string,
	sessionId: string,
	limit = 100
): Promise<SessionSnapshot> {
	const payload = await apiRequest(`/sessions/${encodeURIComponent(sessionId)}/snapshot`, {
		token,
		query: { limit }
	});
	const snapshot = parseSnapshot(payload);
	if (!snapshot) throw new Error('会话快照格式无法识别。');
	return snapshot;
}

export async function fetchEvents(
	token: string,
	sessionId: string,
	afterSeq: number
): Promise<EventRecovery> {
	const payload = await apiRequest(`/sessions/${encodeURIComponent(sessionId)}/events`, {
		token,
		query: { after: seqToCursor(afterSeq) }
	});
	return parseRecovery(payload);
}

export async function createSessionWsTicket(token: string, sessionId: string): Promise<string> {
	const payload = await apiRequest('/ws-ticket', {
		method: 'POST',
		token,
		body: { clientId: clientId(), scope: { sessionId } }
	});
	const ticket = asString(asRecord(payload)?.ticket);
	if (!ticket) throw new Error('未取得 WebSocket 票据。');
	return ticket;
}

export function sessionSocketUrl(sessionId: string, ticket: string): string {
	return webSocketUrl(`/sessions/${encodeURIComponent(sessionId)}/ws`, ticket);
}

export async function sendSessionMessage(
	token: string,
	sessionId: string,
	content: string,
	messageClientId: string
): Promise<void> {
	await apiRequest(`/sessions/${encodeURIComponent(sessionId)}/runtime/messages`, {
		method: 'POST',
		token,
		body: { content, clientMessageId: messageClientId }
	});
}

export async function listNotices(token: string, sessionId: string): Promise<RuntimeNotice[]> {
	const payload = await apiRequest(`/sessions/${encodeURIComponent(sessionId)}/runtime/notices`, {
		token
	});
	return parseNotices(payload);
}

export async function respondNotice(
	token: string,
	sessionId: string,
	noticeId: string,
	decision: NoticeDecision
): Promise<RuntimeNotice | null> {
	const payload = await apiRequest(
		`/sessions/${encodeURIComponent(sessionId)}/notices/${encodeURIComponent(noticeId)}/respond`,
		{ method: 'POST', token, body: { decision } }
	);
	return parseNotice(asRecord(payload)?.notice);
}

export async function interruptSession(token: string, sessionId: string): Promise<string | null> {
	const payload = await apiRequest(`/sessions/${encodeURIComponent(sessionId)}/runtime/interrupt`, {
		method: 'POST',
		token
	});
	return asString(asRecord(payload)?.streamId) || null;
}

export type CreateAndStartInput = {
	connectorId: string;
	projectId: string;
	runtime: string;
	content: string;
	clientMessageId: string;
};

export async function createAndStartSession(
	token: string,
	input: CreateAndStartInput
): Promise<CreateAndStartResult> {
	const payload = await apiRequest('/sessions/create-and-start', {
		method: 'POST',
		token,
		body: input
	});
	const result = parseCreateAndStart(payload);
	if (!result) throw new Error('创建会话的响应格式无法识别。');
	return result;
}

export { cursorToSeq, seqToCursor };
