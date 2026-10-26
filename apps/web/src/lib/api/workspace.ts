import { ApiError, webSocketUrl, apiRequest, clientId } from './client';
import {
	asNumber,
	asRecord,
	asString,
	cursorToSeq,
	parseConnector,
	parseConnectors,
	parseFsListing,
	parseFsText,
	sortFsEntries,
	parseCreateAndStart,
	parseNotice,
	parseNotices,
	parseProject,
	parseProjects,
	parseRecovery,
	parseSessions,
	parseTerminalSession,
	parseSnapshot,
	seqToCursor,
	type Connector,
	type CreateAndStartResult,
	type EventRecovery,
	type FsFailure,
	type FsListing,
	type FsTextFile,
	type NoticeActionId,
	type NoticeInputData,
	type Project,
	type RuntimeNotice,
	type Session,
	type SessionSnapshot,
	type TerminalSession
} from './types';

export async function listConnectors(token: string): Promise<Connector[]> {
	return parseConnectors(await apiRequest('/connectors', { token }));
}

export type CreatedConnector = {
	connector: Connector;
	/** 明文 token：hub 只存 sha256，这个值只在这一条响应里出现一次。 */
	connectorToken: string;
};

export async function createConnector(token: string, name: string): Promise<CreatedConnector> {
	const payload = await apiRequest('/connectors', { method: 'POST', token, body: { name } });
	const raw = asRecord(payload);
	const connector = parseConnector(raw?.connector ?? payload);
	if (!connector) throw new Error('创建设备的响应里没有可识别的 connector。');
	const connectorToken = asString(raw?.connectorToken).trim();
	if (connectorToken.length === 0)
		throw new Error('hub 没有返回 connectorToken：明文 token 只在那一次响应里，请重新创建设备。');
	return { connector, connectorToken };
}

export async function listProjects(token: string): Promise<Project[]> {
	return parseProjects(await apiRequest('/projects', { token }));
}

function requireProject(payload: unknown): Project {
	const project = parseProject(asRecord(payload)?.project ?? payload);
	if (!project) throw new Error('项目响应格式无法识别。');
	return project;
}

export type CreateProjectInput = {
	connectorId: string;
	workspacePath: string;
	name: string;
};

export async function createProject(token: string, input: CreateProjectInput): Promise<Project> {
	return requireProject(
		await apiRequest('/projects', {
			method: 'POST',
			token,
			body: {
				name: input.name,
				connectorId: input.connectorId,
				workspacePath: input.workspacePath,
				manuallyCreated: true
			}
		})
	);
}

export type UpdateProjectInput = { name?: string; pinned?: boolean };

export async function updateProject(
	token: string,
	projectId: string,
	input: UpdateProjectInput
): Promise<Project> {
	const body: Record<string, unknown> = {};
	if (input.name !== undefined) body.name = input.name;
	if (input.pinned !== undefined) body.pinned = input.pinned;
	return requireProject(
		await apiRequest(`/projects/${encodeURIComponent(projectId)}`, {
			method: 'PATCH',
			token,
			body
		})
	);
}

export type DeletedProject = { projectId: string; detachedSessions: number };

export async function deleteProject(token: string, projectId: string): Promise<DeletedProject> {
	const payload = await apiRequest(`/projects/${encodeURIComponent(projectId)}`, {
		method: 'DELETE',
		token
	});
	const raw = asRecord(payload);
	const echoedId = asString(raw?.projectId);
	if (echoedId.length === 0) throw new Error('删除项目的响应里没有 projectId。');
	return { projectId: echoedId, detachedSessions: asNumber(raw?.detachedSessions) };
}

export async function listProjectSessions(
	token: string,
	projectId: string
): Promise<Session[]> {
	return parseSessions(
		await apiRequest(`/projects/${encodeURIComponent(projectId)}/sessions`, { token })
	);
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
	actionId: NoticeActionId,
	inputData: NoticeInputData | null
): Promise<RuntimeNotice | null> {
	// 契约字段是 actionId；decision 是后端仍兼容的旧字段，留着让未升级的 hub 也认三个旧值。
	const body: Record<string, unknown> = { actionId, decision: actionId };
	if (inputData !== null) body.inputData = inputData;
	const payload = await apiRequest(
		`/sessions/${encodeURIComponent(sessionId)}/notices/${encodeURIComponent(noticeId)}/respond`,
		{ method: 'POST', token, body }
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

export async function listSessionDir(
	token: string,
	sessionId: string,
	path: string
): Promise<FsListing> {
	const payload = await apiRequest(`/sessions/${encodeURIComponent(sessionId)}/files/list`, {
		method: 'POST',
		token,
		body: { path }
	});
	const listing = parseFsListing(payload);
	if (!listing) throw new Error('目录列表格式无法识别。');
	return { ...listing, entries: sortFsEntries(listing.entries) };
}

export async function readSessionText(
	token: string,
	sessionId: string,
	path: string
): Promise<FsTextFile> {
	const payload = await apiRequest(
		`/sessions/${encodeURIComponent(sessionId)}/files/readText`,
		{ method: 'POST', token, body: { path } }
	);
	const file = parseFsText(payload);
	if (!file) throw new Error('文件内容的响应格式无法识别。');
	return file;
}

export function fsFailure(error: unknown): FsFailure {
	if (error instanceof ApiError) {
		return { status: error.status, code: error.code, message: error.message };
	}
	if (error instanceof Error) {
		return { status: 0, code: null, message: error.message };
	}
	return { status: 0, code: null, message: typeof error === 'string' ? error : '未知错误' };
}

export { cursorToSeq, seqToCursor };

export type OpenTerminalInput = {
	cwd?: string;
	cols: number;
	rows: number;
};

export async function openSessionTerminal(
	token: string,
	sessionId: string,
	input: OpenTerminalInput
): Promise<TerminalSession> {
	const body: Record<string, unknown> = { cols: input.cols, rows: input.rows };
	const cwd = input.cwd?.trim() ?? '';
	if (cwd.length > 0) body.cwd = cwd;
	const payload = await apiRequest(`/sessions/${encodeURIComponent(sessionId)}/terminal`, {
		method: 'POST',
		token,
		body
	});
	const created = parseTerminalSession(payload);
	if (!created) throw new Error('创建终端的响应格式无法识别。');
	return created;
}

// 终端流按 terminalId 单独取票：票据一次性读即删，复用会话票据会让终端握手跨作用域。
export async function createTerminalWsTicket(token: string, terminalId: string): Promise<string> {
	const payload = await apiRequest('/ws-ticket', {
		method: 'POST',
		token,
		body: { clientId: clientId(), scope: { terminalId } }
	});
	const ticket = asString(asRecord(payload)?.ticket);
	if (!ticket) throw new Error('未取得 WebSocket 票据。');
	return ticket;
}

export function sessionTerminalSocketUrl(
	sessionId: string,
	terminalId: string,
	ticket: string
): string {
	return webSocketUrl(
		`/sessions/${encodeURIComponent(sessionId)}/terminal/${encodeURIComponent(terminalId)}/stream`,
		ticket
	);
}

export const terminalFailure = fsFailure;
