export function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

export function asString(value: unknown, fallback = ''): string {
	return typeof value === 'string' ? value : fallback;
}

export function asNullableString(value: unknown): string | null {
	return typeof value === 'string' ? value : null;
}

export function asNumber(value: unknown, fallback = 0): number {
	return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

export function asBoolean(value: unknown, fallback = false): boolean {
	return typeof value === 'boolean' ? value : fallback;
}

export function asArray(value: unknown): unknown[] {
	return Array.isArray(value) ? value : [];
}

function parseList<T>(value: unknown, key: string, parse: (raw: unknown) => T | null): T[] {
	const record = asRecord(value);
	const rows = asArray(record?.[key] ?? value);
	const parsed: T[] = [];
	for (const row of rows) {
		const item = parse(row);
		if (item) parsed.push(item);
	}
	return parsed;
}

export type AuthConfig = {
	needsBootstrap: boolean;
	registrationOpen: boolean;
	emailVerificationRequired: boolean;
	oauthEnabled: boolean;
	serverTime: string;
};

export function parseAuthConfig(value: unknown): AuthConfig | null {
	const raw = asRecord(value);
	if (!raw) return null;
	return {
		needsBootstrap: asBoolean(raw.needsBootstrap),
		registrationOpen: asBoolean(raw.registrationOpen),
		emailVerificationRequired: asBoolean(raw.emailVerificationRequired),
		oauthEnabled: asBoolean(raw.oauthEnabled),
		serverTime: asString(raw.serverTime)
	};
}

export type AuthUser = {
	userId: string;
	email: string | null;
	displayName: string;
	role: string;
	disabled: boolean;
};

export function parseAuthUser(value: unknown): AuthUser | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const userId = asString(raw.userId);
	if (!userId) return null;
	return {
		userId,
		email: asNullableString(raw.email),
		displayName: asString(raw.displayName, userId),
		role: asString(raw.role, 'member'),
		disabled: asBoolean(raw.disabled)
	};
}

export type Connector = {
	id: string;
	name: string;
	connectorKind: string;
	deviceOs: string;
	status: string;
	lastSeenAt: string | null;
};

export function parseConnector(value: unknown): Connector | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const id = asString(raw.id);
	if (!id) return null;
	return {
		id,
		name: asString(raw.name, id),
		connectorKind: asString(raw.connectorKind, 'cli'),
		deviceOs: asString(raw.deviceOs),
		status: asString(raw.status, 'offline'),
		lastSeenAt: asNullableString(raw.lastSeenAt)
	};
}

export function parseConnectors(value: unknown): Connector[] {
	return parseList(value, 'connectors', parseConnector);
}

export type Project = {
	id: string;
	connectorId: string;
	name: string;
	workspacePath: string;
	activeSessionCount: number;
	lastActivityAt: string | null;
};

export function parseProject(value: unknown): Project | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const id = asString(raw.id);
	if (!id) return null;
	return {
		id,
		connectorId: asString(raw.connectorId),
		name: asString(raw.name, id),
		workspacePath: asString(raw.workspacePath),
		activeSessionCount: asNumber(raw.activeSessionCount),
		lastActivityAt: asNullableString(raw.lastActivityAt)
	};
}

export function parseProjects(value: unknown): Project[] {
	return parseList(value, 'projects', parseProject);
}

export type Session = {
	id: string;
	connectorId: string;
	projectId: string | null;
	runtime: string;
	status: string;
	title: string | null;
	connectorStatus: string;
	archived: boolean;
	unread: boolean;
	updatedSeq: number;
	lastActivityAt: string | null;
};

export function parseSession(value: unknown): Session | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const id = asString(raw.id);
	if (!id) return null;
	return {
		id,
		connectorId: asString(raw.connectorId),
		projectId: asNullableString(raw.projectId),
		runtime: asString(raw.runtime, 'unknown'),
		status: asString(raw.status, 'idle'),
		title: asNullableString(raw.title),
		connectorStatus: asString(raw.connectorStatus, 'unknown'),
		archived: asBoolean(raw.archived),
		unread: asBoolean(raw.unread),
		updatedSeq: asNumber(raw.updatedSeq),
		lastActivityAt: asNullableString(raw.lastActivityAt)
	};
}

export function parseSessions(value: unknown): Session[] {
	return parseList(value, 'sessions', parseSession);
}

export type RuntimeState = {
	sessionId: string;
	runtime: string;
	status: string;
	statusReason: string | null;
	updatedSeq: number;
};

export function parseRuntimeState(value: unknown): RuntimeState | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const sessionId = asString(raw.sessionId);
	if (!sessionId) return null;
	return {
		sessionId,
		runtime: asString(raw.runtime, 'unknown'),
		status: asString(raw.status, 'idle'),
		statusReason: asNullableString(raw.statusReason),
		updatedSeq: asNumber(raw.updatedSeq)
	};
}

export type TimelineItemType = 'message' | 'tool' | 'artifact' | 'marker' | 'system';
export type TimelineItemStatus =
	| 'pending'
	| 'running'
	| 'waiting_approval'
	| 'done'
	| 'failed'
	| 'cancelled'
	| 'interrupted';

const ITEM_TYPES: readonly string[] = ['message', 'tool', 'artifact', 'marker', 'system'];
const ITEM_STATUSES: readonly string[] = [
	'pending',
	'running',
	'waiting_approval',
	'done',
	'failed',
	'cancelled',
	'interrupted'
];

function narrowItemType(value: unknown): TimelineItemType {
	const raw = asString(value);
	return ITEM_TYPES.includes(raw) ? (raw as TimelineItemType) : 'system';
}

function narrowItemStatus(value: unknown): TimelineItemStatus {
	const raw = asString(value);
	return ITEM_STATUSES.includes(raw) ? (raw as TimelineItemStatus) : 'pending';
}

export type TimelineSource = {
	runtime: string;
	clientMessageId: string | null;
	itemType: string | null;
	event: string | null;
};

export type TimelineItem = {
	id: string;
	sessionId: string;
	type: TimelineItemType;
	status: TimelineItemStatus;
	role: string | null;
	orderSeq: number;
	updatedSeq: number;
	revision: number;
	createdAt: string;
	content: Record<string, unknown>;
	source: TimelineSource;
};

export function parseTimelineSource(value: unknown): TimelineSource {
	const raw = asRecord(value) ?? {};
	return {
		runtime: asString(raw.runtime, 'unknown'),
		clientMessageId: asNullableString(raw.clientMessageId),
		itemType: asNullableString(raw.itemType),
		event: asNullableString(raw.event)
	};
}

export function parseTimelineItem(value: unknown): TimelineItem | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const id = asString(raw.id);
	if (!id) return null;
	return {
		id,
		sessionId: asString(raw.sessionId),
		type: narrowItemType(raw.type),
		status: narrowItemStatus(raw.status),
		role: asNullableString(raw.role),
		orderSeq: asNumber(raw.orderSeq),
		updatedSeq: asNumber(raw.updatedSeq),
		revision: asNumber(raw.revision, 1),
		createdAt: asString(raw.createdAt),
		content: asRecord(raw.content) ?? {},
		source: parseTimelineSource(raw.source)
	};
}

export function parseTimelineItems(value: unknown): TimelineItem[] {
	const parsed: TimelineItem[] = [];
	for (const row of asArray(value)) {
		const item = parseTimelineItem(row);
		if (item) parsed.push(item);
	}
	return parsed;
}

export type SessionSnapshot = {
	session: Session;
	state: RuntimeState | null;
	items: TimelineItem[];
	nextSeq: number;
	hasMore: boolean;
	eventCursor: string;
	serverTime: string;
	approvalCount: number;
	noticeCount: number;
};

export function parseSnapshot(value: unknown): SessionSnapshot | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const session = parseSession(raw.session);
	if (!session) return null;
	const timeline = asRecord(raw.timeline) ?? {};
	return {
		session,
		state: parseRuntimeState(raw.state),
		items: parseTimelineItems(timeline.items),
		nextSeq: asNumber(timeline.nextSeq),
		hasMore: asBoolean(timeline.hasMore),
		eventCursor: asString(raw.eventCursor),
		serverTime: asString(raw.serverTime),
		approvalCount: asArray(raw.approvals).length,
		noticeCount: asArray(raw.notices).length
	};
}

export type SessionEvent = {
	type: string;
	sessionId: string;
	sequence: number;
	cursor: string;
	payload: Record<string, unknown>;
};

export function parseEvent(value: unknown): SessionEvent | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const type = asString(raw.type);
	if (!type) return null;
	return {
		type,
		sessionId: asString(raw.sessionId),
		sequence: asNumber(raw.sequence, -1),
		cursor: asString(raw.cursor),
		payload: asRecord(raw.payload) ?? {}
	};
}

export type EventRecovery = {
	events: SessionEvent[];
	nextCursor: string;
	snapshotRequired: boolean;
};

export function parseRecovery(value: unknown): EventRecovery {
	const raw = asRecord(value) ?? {};
	const events: SessionEvent[] = [];
	for (const row of asArray(raw.events)) {
		const event = parseEvent(row);
		if (event) events.push(event);
	}
	return {
		events,
		nextCursor: asString(raw.nextCursor),
		snapshotRequired: asBoolean(raw.snapshotRequired)
	};
}

export function cursorToSeq(cursor: string | null | undefined): number | null {
	if (!cursor) return null;
	const match = /^seq:(\d+)$/.exec(cursor.trim());
	if (!match) return null;
	const seq = Number.parseInt(match[1] ?? '', 10);
	return Number.isFinite(seq) ? seq : null;
}

export function seqToCursor(seq: number): string {
	return `seq:${Math.max(0, Math.trunc(seq))}`;
}
