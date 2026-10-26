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

/** 设备自己上报的运行时探测结果；available 为 false 时 reason 是设备给的原因原文。 */
export type ConnectorRuntime = {
	name: string;
	available: boolean;
	reason: string | null;
};

export function parseConnectorRuntime(value: unknown): ConnectorRuntime | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const name = asString(raw.name).trim();
	if (!name) return null;
	return {
		name,
		available: asBoolean(raw.available),
		reason: asNullableString(raw.reason)
	};
}

export type Connector = {
	id: string;
	name: string;
	connectorKind: string;
	deviceOs: string;
	status: string;
	lastSeenAt: string | null;
	/** 缺键与空数组同义：设备没有上报，等于「未知」，不是「全部可用」。 */
	runtimes: ConnectorRuntime[];
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
		lastSeenAt: asNullableString(raw.lastSeenAt),
		runtimes: parseList(raw, 'runtimes', parseConnectorRuntime)
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
	pinned: boolean;
	pinnedAt: string | null;
	manuallyCreated: boolean;
	createdAt: string | null;
	updatedAt: string | null;
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
		lastActivityAt: asNullableString(raw.lastActivityAt),
		pinned: asBoolean(raw.pinned),
		pinnedAt: asNullableString(raw.pinnedAt),
		manuallyCreated: asBoolean(raw.manuallyCreated, true),
		createdAt: asNullableString(raw.createdAt),
		updatedAt: asNullableString(raw.updatedAt)
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
	cwd: string | null;
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
		cwd: asNullableString(raw.cwd),
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
	selections: RuntimeSelections;
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
		selections: parseRuntimeSelections(raw.selections) ?? NO_RUNTIME_SELECTIONS,
		updatedSeq: asNumber(raw.updatedSeq)
	};
}

/** 操作 id 是自由字符串（DSH 的 actionId 直传），旧的 approve/approve_for_session/reject 只是它的一个子集。 */
export type NoticeActionId = string;

export type NoticeOption = {
	id: string;
	/** 旧字符串形态只带 id，label 为空，由渲染层回落。 */
	label: string;
	style: string | null;
};

export type NoticeInputData = { text: string };

export type RuntimeNotice = {
	id: string;
	kind: string;
	status: string;
	title: string;
	body: Record<string, unknown> | null;
	options: NoticeOption[];
	/** 'approval' | 'question'；缺省时不猜，交给渲染层按审批处理。 */
	interactionType: string | null;
	decision: string | null;
	createdAt: string;
	updatedAt: string;
};

/** options 两种形态并存：旧数据是 id 字符串数组，新数据是 {id,label,style}。 */
function parseNoticeOption(value: unknown): NoticeOption | null {
	if (typeof value === 'string') {
		const id = value.trim();
		return id.length > 0 ? { id, label: '', style: null } : null;
	}
	const raw = asRecord(value);
	if (!raw) return null;
	const id = (asString(raw.id) || asString(raw.actionId)).trim();
	if (id.length === 0) return null;
	return { id, label: asString(raw.label).trim(), style: asNullableString(raw.style) };
}

export function parseNotice(value: unknown): RuntimeNotice | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const id = asString(raw.id);
	if (!id) return null;
	return {
		id,
		kind: asString(raw.kind, 'unknown'),
		status: asString(raw.status, 'open'),
		title: asString(raw.title),
		body: asRecord(raw.body),
		options: asArray(raw.options)
			.map(parseNoticeOption)
			.filter((option): option is NoticeOption => option !== null),
		interactionType: asNullableString(raw.interactionType),
		decision: asNullableString(raw.decision),
		createdAt: asString(raw.createdAt),
		updatedAt: asString(raw.updatedAt)
	};
}

export function parseNotices(value: unknown): RuntimeNotice[] {
	return parseList(value, 'notices', parseNotice);
}

export function isOpenNotice(notice: RuntimeNotice): boolean {
	return notice.status !== 'resolved';
}

export type ConnectorResult = {
	ok: boolean;
	code: string | null;
	streamId: string | null;
	sessionId: string | null;
};

export function parseConnectorResult(value: unknown): ConnectorResult | null {
	const raw = asRecord(value);
	if (!raw) return null;
	return {
		ok: asBoolean(raw.ok),
		code: asNullableString(raw.code),
		streamId: asNullableString(raw.streamId),
		sessionId: asNullableString(raw.sessionId)
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
		// 公开分享只暴露白名单字段，时间叫 `itemTime`；普通快照仍是 `createdAt`。
		createdAt: asString(raw.createdAt) || asString(raw.itemTime),
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
	notices: RuntimeNotice[];
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
		notices: parseNotices(raw.notices)
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

export type CreateAndStartResult = {
	session: Session;
	connectorResult: ConnectorResult | null;
};

export function parseCreateAndStart(value: unknown): CreateAndStartResult | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const session = parseSession(raw.session);
	if (!session) return null;
	return { session, connectorResult: parseConnectorResult(raw.connectorResult) };
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

export type FsEntryKind = 'file' | 'dir';

export type FsEntry = {
	name: string;
	path: string;
	kind: FsEntryKind;
	sizeBytes: number;
	modifiedAt: string | null;
};

export type FsListing = {
	path: string;
	entries: FsEntry[];
	serverTime: string;
};

export type FsTextFile = {
	path: string;
	content: string;
	sizeBytes: number;
	truncated: boolean;
	serverTime: string;
};

export type FsFailure = {
	status: number;
	code: string | null;
	message: string;
};

function narrowFsKind(value: unknown): FsEntryKind | null {
	if (value === 'file' || value === 'dir') return value;
	return null;
}

export function parseFsEntry(value: unknown): FsEntry | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const path = asString(raw.path);
	const kind = narrowFsKind(raw.kind);
	if (!path || !kind) return null;
	const name = asString(raw.name);
	return {
		name: name.length > 0 ? name : (path.split('/').pop() ?? path),
		path,
		kind,
		sizeBytes: asNumber(raw.sizeBytes),
		modifiedAt: asNullableString(raw.modifiedAt)
	};
}

export function parseFsListing(value: unknown): FsListing | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const path = asString(raw.path);
	if (!path) return null;
	const entries: FsEntry[] = [];
	for (const row of asArray(raw.entries)) {
		const entry = parseFsEntry(row);
		if (entry) entries.push(entry);
	}
	return { path, entries, serverTime: asString(raw.serverTime) };
}

export function parseFsText(value: unknown): FsTextFile | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const path = asString(raw.path);
	if (!path) return null;
	return {
		path,
		content: asString(raw.content),
		sizeBytes: asNumber(raw.sizeBytes),
		truncated: asBoolean(raw.truncated),
		serverTime: asString(raw.serverTime)
	};
}

export function sortFsEntries(entries: FsEntry[]): FsEntry[] {
	return [...entries].sort((left, right) => {
		if (left.kind !== right.kind) return left.kind === 'dir' ? -1 : 1;
		return left.name.localeCompare(right.name, 'zh-CN');
	});
}


export type TerminalInfo = {
	id: string;
	cwd: string | null;
	cols: number;
	rows: number;
	createdAt: string | null;
};

export function parseTerminalInfo(value: unknown): TerminalInfo | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const id = asString(raw.id);
	if (!id) return null;
	const cols = Math.trunc(asNumber(raw.cols, 80));
	const rows = Math.trunc(asNumber(raw.rows, 24));
	return {
		id,
		cwd: asNullableString(raw.cwd),
		cols: cols > 0 ? cols : 80,
		rows: rows > 0 ? rows : 24,
		createdAt: asNullableString(raw.createdAt)
	};
}

export type TerminalSession = {
	terminal: TerminalInfo;
	serverTime: string;
};

export function parseTerminalSession(value: unknown): TerminalSession | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const terminal = parseTerminalInfo(raw.terminal);
	if (!terminal) return null;
	return { terminal, serverTime: asString(raw.serverTime) };
}

export type TerminalServerFrame =
	| { type: 'terminal.opened' }
	| { type: 'terminal.data'; data: string }
	| { type: 'terminal.exit'; code: number | null }
	| { type: 'terminal.error'; message: string; code: string | null };

export type TerminalClientFrame =
	| { type: 'terminal.input'; data: string }
	| { type: 'terminal.resize'; cols: number; rows: number }
	| { type: 'terminal.close' };

export function parseTerminalFrame(value: unknown): TerminalServerFrame | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const type = asString(raw.type);
	switch (type) {
		case 'terminal.opened':
			return { type };
		case 'terminal.data': {
			const data = asString(raw.data);
			return data.length > 0 ? { type, data } : null;
		}
		case 'terminal.exit': {
			const code =
				typeof raw.code === 'number' && Number.isFinite(raw.code) ? Math.trunc(raw.code) : null;
			return { type, code };
		}
		case 'terminal.error':
			return {
				type,
				message: asString(raw.message, '终端出错'),
				code: asNullableString(raw.code)
			};
		default:
			return null;
	}
}

export type ShareScope = 'session' | 'message';

function narrowShareScope(value: unknown): ShareScope | null {
	const raw = asString(value);
	return raw === 'session' || raw === 'message' ? raw : null;
}

export type SessionShare = {
	id: string;
	scope: ShareScope;
	urlPath: string;
	createdAt: string;
};

export function parseSessionShare(value: unknown): SessionShare | null {
	const raw = asRecord(asRecord(value)?.share ?? value);
	if (!raw) return null;
	const id = asString(raw.id);
	const scope = narrowShareScope(raw.scope);
	if (!id || !scope) return null;
	return { id, scope, urlPath: asString(raw.urlPath), createdAt: asString(raw.createdAt) };
}

export type PublicShare = {
	id: string;
	scope: ShareScope;
	sessionTitle: string;
	createdAt: string;
	items: TimelineItem[];
};

export function parsePublicShare(value: unknown): PublicShare | null {
	const raw = asRecord(asRecord(value)?.share ?? value);
	if (!raw) return null;
	const id = asString(raw.id);
	const scope = narrowShareScope(raw.scope);
	if (!id || !scope) return null;
	const timeline = asRecord(raw.timeline) ?? {};
	return {
		id,
		scope,
		sessionTitle: asString(raw.sessionTitle),
		createdAt: asString(raw.createdAt),
		items: parseTimelineItems(timeline.items)
	};
}


export type AdminRole = 'admin' | 'member';

export const ADMIN_ROLES: readonly AdminRole[] = ['admin', 'member'];

export function isAdminRole(value: unknown): value is AdminRole {
	return value === 'admin' || value === 'member';
}

export type AdminUser = {
	id: string;
	email: string | null;
	displayName: string;
	role: AdminRole;
	disabled: boolean;
	createdAt: string;
};

export function parseAdminUser(value: unknown): AdminUser | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const id = asString(raw.id);
	if (!id) return null;
	return {
		id,
		email: asNullableString(raw.email),
		displayName: asString(raw.displayName, id),
		role: isAdminRole(raw.role) ? raw.role : 'member',
		disabled: asBoolean(raw.disabled),
		createdAt: asString(raw.createdAt)
	};
}

export type AdminUserPage = {
	users: AdminUser[];
	nextCursor: string | null;
};

export function parseAdminUserPage(value: unknown): AdminUserPage | null {
	const raw = asRecord(value);
	if (!raw || !Array.isArray(raw.users)) return null;
	const cursor = asString(raw.nextCursor).trim();
	return {
		users: parseList(raw, 'users', parseAdminUser),
		nextCursor: cursor.length > 0 ? cursor : null
	};
}

export type AdminSettings = {
	registrationOpen: boolean;
	emailVerificationRequired: boolean;
};

// 后端既可能把设置包在 settings 里，也可能直接平铺；两种都收，缺字段按 false 处理。
export function parseAdminSettings(value: unknown): AdminSettings | null {
	const raw = asRecord(asRecord(value)?.settings ?? value);
	if (!raw) return null;
	return {
		registrationOpen: asBoolean(raw.registrationOpen),
		emailVerificationRequired: asBoolean(raw.emailVerificationRequired)
	};
}

export type RuntimeCommand = {
	name: string;
	description: string;
};

/** 运行时自己的命令名可能带前导 `/`；界面统一按不带斜杠的名字显示，选中时再补回去。 */
export function parseRuntimeCommand(value: unknown): RuntimeCommand | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const name = asString(raw.name).trim().replace(/^\/+/, '');
	if (!name) return null;
	return { name, description: asString(raw.description) };
}

export function parseRuntimeCommands(value: unknown): RuntimeCommand[] {
	const commands: RuntimeCommand[] = [];
	for (const row of asArray(asRecord(value)?.commands ?? value)) {
		const command = parseRuntimeCommand(row);
		if (command) commands.push(command);
	}
	return commands;
}

export type RuntimeCatalogItem = {
	id: string;
	name: string;
	description: string;
};

export function parseRuntimeCatalogItem(value: unknown): RuntimeCatalogItem | null {
	const raw = asRecord(value);
	if (!raw) return null;
	const id = asString(raw.id).trim();
	if (!id) return null;
	return { id, name: asString(raw.name).trim() || id, description: asString(raw.description) };
}

export function parseRuntimeCatalog(value: unknown): RuntimeCatalogItem[] {
	const items: RuntimeCatalogItem[] = [];
	for (const row of asArray(asRecord(value)?.items ?? value)) {
		const item = parseRuntimeCatalogItem(row);
		if (item) items.push(item);
	}
	return items;
}

export type RuntimeCatalogs = {
	models: RuntimeCatalogItem[];
	permissions: RuntimeCatalogItem[];
};

export type RuntimeCapabilityFlags = {
	steer: boolean;
	interrupt: boolean;
	selections: boolean;
	commands: boolean;
};

/** 能力没拿到时用它：每一项都关，界面就不会给出后端做不到的入口。 */
export const NO_RUNTIME_CAPABILITIES: RuntimeCapabilityFlags = {
	steer: false,
	interrupt: false,
	selections: false,
	commands: false
};

export type RuntimeCapabilities = {
	flags: RuntimeCapabilityFlags;
	revision: number;
	serverTime: string;
};

/**
 * 能力是运行时自己报的：没报过就是全 false。两种键名都收 —— `capabilitySet` 是
 * hub 落地的写法，`capabilities` 是接口草案的写法；两个都没有就返回 null，
 * 让调用方显示真实错误，而不是猜「全支持」。
 */
export function parseRuntimeCapabilities(value: unknown): RuntimeCapabilities | null {
	const raw = asRecord(value);
	const flags = asRecord(raw?.capabilitySet) ?? asRecord(raw?.capabilities);
	if (!raw || !flags) return null;
	return {
		flags: {
			steer: asBoolean(flags.steer),
			interrupt: asBoolean(flags.interrupt),
			selections: asBoolean(flags.selections),
			commands: asBoolean(flags.commands)
		},
		revision: asNumber(raw.revision),
		serverTime: asString(raw.serverTime)
	};
}

export type RuntimeSelections = {
	model: string | null;
	permissionMode: string | null;
};

export const NO_RUNTIME_SELECTIONS: RuntimeSelections = { model: null, permissionMode: null };

export function parseRuntimeSelections(value: unknown): RuntimeSelections | null {
	const raw = asRecord(asRecord(value)?.selections ?? value);
	if (!raw) return null;
	return {
		model: asNullableString(raw.model),
		permissionMode: asNullableString(raw.permissionMode)
	};
}

export function parseSteerDelivery(value: unknown): boolean {
	return asBoolean(asRecord(value)?.delivered);
}
