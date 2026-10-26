import {
	createSessionWsTicket,
	fetchEvents,
	fetchSnapshot,
	interruptSession,
	respondNotice,
	sendSessionMessage,
	sessionSocketUrl
} from './api/workspace';
import {
	asString,
	cursorToSeq,
	isOpenNotice,
	parseEvent,
	parseNotice,
	parseNotices,
	parseRuntimeState,
	parseSession,
	parseTimelineItem,
	parseTimelineItems,
	type NoticeDecision,
	type RuntimeNotice,
	type RuntimeState,
	type Session,
	type SessionEvent,
	type TimelineItem
} from './api/types';
import { describeError } from './format';
import { noticeItemId } from './notice';
import { isVisibleItem, sortItems } from './timeline';

export type NoticeAction = {
	pending: boolean;
	submitted: NoticeDecision | null;
	error: string | null;
	lastDecision: NoticeDecision | null;
};

const EMPTY_ACTION: NoticeAction = {
	pending: false,
	submitted: null,
	error: null,
	lastDecision: null
};

export type ConnectionState = 'idle' | 'connecting' | 'open' | 'reconnecting' | 'closed';

export type OptimisticMessage = {
	clientMessageId: string;
	content: string;
	createdAt: string;
	failed: boolean;
	error: string | null;
};

const MAX_BACKOFF_MS = 8_000;
const SNAPSHOT_LIMIT = 200;

export class SessionController {
	sessionId = $state('');
	session = $state<Session | null>(null);
	runtimeState = $state<RuntimeState | null>(null);
	items = $state<TimelineItem[]>([]);
	optimistic = $state<OptimisticMessage[]>([]);
	connection = $state<ConnectionState>('idle');
	loading = $state(true);
	error = $state<string | null>(null);
	approvalCount = $state(0);
	notices = $state<RuntimeNotice[]>([]);
	/** Notices the runtime closed while this page was open; those stay on screen, greyed. */
	resolvedWhileOpen = $state<string[]>([]);
	noticeActions = $state<Record<string, NoticeAction>>({});
	interrupting = $state(false);
	interruptStream = $state<string | null>(null);
	interruptError = $state<string | null>(null);
	lastEventAt = $state<number | null>(null);
	snapshotCount = $state(0);

	#token = '';
	cursorSeq = $state(0);
	#socket: WebSocket | null = null;
	#timer: ReturnType<typeof setTimeout> | null = null;
	#refetchTimer: ReturnType<typeof setTimeout> | null = null;
	#attempt = 0;
	#disposed = true;

	get visibleItems(): TimelineItem[] {
		return this.items.filter(isVisibleItem);
	}

	/** Notices the runtime has not closed yet — the only ones that can be answered. */
	get openNotices(): RuntimeNotice[] {
		return this.notices.filter(isOpenNotice);
	}

	/** Open notices plus the ones this page watched get resolved. */
	get approvalNotices(): RuntimeNotice[] {
		const watched = new Set(this.resolvedWhileOpen);
		return this.notices.filter((notice) => isOpenNotice(notice) || watched.has(notice.id));
	}

	/**
	 * Items the runtime parked on `waiting_approval` with no notice to answer.
	 * Shown as a read-only card rather than buttons that would have nothing to POST.
	 */
	get pendingApprovalItems(): TimelineItem[] {
		const covered = new Set(
			this.openNotices
				.map(noticeItemId)
				.filter((value): value is string => value !== null)
		);
		return this.items.filter(
			(item) => item.status === 'waiting_approval' && !covered.has(item.id)
		);
	}

	noticeAction(noticeId: string): NoticeAction {
		return this.noticeActions[noticeId] ?? EMPTY_ACTION;
	}

	async open(sessionId: string, token: string): Promise<void> {
		this.dispose();
		this.#disposed = false;
		this.sessionId = sessionId;
		this.#token = token;
		this.cursorSeq = 0;
		this.#attempt = 0;
		this.loading = true;
		this.error = null;
		this.items = [];
		this.optimistic = [];
		this.notices = [];
		this.resolvedWhileOpen = [];
		this.noticeActions = {};
		this.interrupting = false;
		this.interruptStream = null;
		this.interruptError = null;
		this.session = null;
		this.runtimeState = null;
		this.connection = 'idle';
		try {
			await this.refreshSnapshot();
		} catch (error) {
			this.error = describeError(error);
			this.loading = false;
			return;
		}
		this.loading = false;
		this.connect();
	}

	dispose(): void {
		this.#disposed = true;
		if (this.#timer !== null) {
			clearTimeout(this.#timer);
			this.#timer = null;
		}
		if (this.#refetchTimer !== null) {
			clearTimeout(this.#refetchTimer);
			this.#refetchTimer = null;
		}
		const socket = this.#socket;
		this.#socket = null;
		if (socket) {
			socket.onopen = null;
			socket.onmessage = null;
			socket.onclose = null;
			socket.close();
		}
	}

	async refreshSnapshot(): Promise<void> {
		const snapshot = await fetchSnapshot(this.#token, this.sessionId, SNAPSHOT_LIMIT);
		this.session = snapshot.session;
		this.runtimeState = snapshot.state;
		this.items = sortItems(snapshot.items);
		this.approvalCount = snapshot.approvalCount;
		this.notices = snapshot.notices;
		this.snapshotCount += 1;
		const cursor = cursorToSeq(snapshot.eventCursor) ?? snapshot.nextSeq;
		this.cursorSeq = Math.max(this.cursorSeq, cursor);
		this.reconcileOptimistic();
	}

	connect(): void {
		if (this.#disposed) return;
		this.connection = this.#attempt === 0 ? 'connecting' : 'reconnecting';
		void this.openSocket();
	}

	async send(content: string): Promise<boolean> {
		const text = content.trim();
		if (text.length === 0) return false;
		const clientMessageId = `cmid-${crypto.randomUUID()}`;
		this.optimistic = [
			...this.optimistic,
			{ clientMessageId, content: text, createdAt: new Date().toISOString(), failed: false, error: null }
		];
		try {
			await sendSessionMessage(this.#token, this.sessionId, text, clientMessageId);
			return true;
		} catch (error) {
			const message = describeError(error);
			this.optimistic = this.optimistic.map((row) =>
				row.clientMessageId === clientMessageId ? { ...row, failed: true, error: message } : row
			);
			return false;
		}
	}

	/**
	 * Send the decision to the hub. The card must not flip to "approved" here: the
	 * hub keeps the notice open until the connector confirms with
	 * `notice.resolved`, which arrives as a `runtime.notice.updated` delta.
	 */
	async respond(noticeId: string, decision: NoticeDecision): Promise<void> {
		if (this.noticeAction(noticeId).pending) return;
		this.setNoticeAction(noticeId, {
			pending: true,
			submitted: null,
			error: null,
			lastDecision: decision
		});
		try {
			await respondNotice(this.#token, this.sessionId, noticeId, decision);
			this.setNoticeAction(noticeId, {
				pending: false,
				submitted: decision,
				error: null,
				lastDecision: decision
			});
		} catch (error) {
			this.setNoticeAction(noticeId, {
				pending: false,
				submitted: null,
				error: describeError(error),
				lastDecision: decision
			});
		}
	}

	/** The session status only changes when the runtime reports it over WS. */
	async interrupt(): Promise<void> {
		if (this.interrupting) return;
		this.interrupting = true;
		this.interruptError = null;
		this.interruptStream = null;
		try {
			this.interruptStream = await interruptSession(this.#token, this.sessionId);
		} catch (error) {
			this.interruptError = describeError(error);
		} finally {
			this.interrupting = false;
		}
	}

	setNoticeAction(noticeId: string, action: NoticeAction): void {
		this.noticeActions = { ...this.noticeActions, [noticeId]: action };
	}

	private upsertNotice(notice: RuntimeNotice): void {
		const next = [...this.notices];
		const index = next.findIndex((existing) => existing.id === notice.id);
		const wasOpen = index >= 0 && isOpenNotice(next[index]);
		if (index >= 0) next[index] = notice;
		else next.push(notice);
		if (wasOpen && !isOpenNotice(notice) && !this.resolvedWhileOpen.includes(notice.id)) {
			this.resolvedWhileOpen = [...this.resolvedWhileOpen, notice.id];
		}
		this.notices = next.sort(
			(a, b) => a.createdAt.localeCompare(b.createdAt) || a.id.localeCompare(b.id)
		);
	}

	discardOptimistic(clientMessageId: string): void {
		this.optimistic = this.optimistic.filter((row) => row.clientMessageId !== clientMessageId);
	}

	private async openSocket(): Promise<void> {
		let ticket: string;
		try {
			ticket = await createSessionWsTicket(this.#token, this.sessionId);
		} catch {
			this.scheduleReconnect();
			return;
		}
		if (this.#disposed) return;
		const socket = new WebSocket(sessionSocketUrl(this.sessionId, ticket));
		this.#socket = socket;
		socket.onopen = () => {
			this.connection = 'open';
			this.#attempt = 0;
			void this.recover();
		};
		socket.onmessage = (event: MessageEvent<unknown>) => {
			this.handleFrame(event.data);
		};
		socket.onclose = () => {
			this.#socket = null;
			if (this.#disposed) return;
			this.scheduleReconnect();
		};
	}

	private scheduleReconnect(): void {
		if (this.#disposed) return;
		this.connection = 'reconnecting';
		const delay = Math.min(MAX_BACKOFF_MS, 500 * 2 ** this.#attempt);
		this.#attempt += 1;
		this.#timer = setTimeout(() => {
			this.#timer = null;
			void this.openSocket();
		}, delay);
	}

	private async recover(): Promise<void> {
		try {
			const recovery = await fetchEvents(this.#token, this.sessionId, this.cursorSeq);
			if (this.#disposed) return;
			if (recovery.snapshotRequired) {
				await this.refreshSnapshot();
				return;
			}
			for (const event of recovery.events) this.applyEvent(event);
			const next = cursorToSeq(recovery.nextCursor);
			if (next !== null) this.cursorSeq = Math.max(this.cursorSeq, next);
		} catch {
			// 下一次重连会再次补拉；此处不覆盖页面级错误
		}
	}

	private handleFrame(data: unknown): void {
		if (typeof data !== 'string') return;
		let parsed: unknown;
		try {
			parsed = JSON.parse(data) as unknown;
		} catch {
			return;
		}
		const event = parseEvent(parsed);
		if (!event) return;
		if (event.type === 'keepalive') return;
		this.applyEvent(event);
	}

	private applyEvent(event: SessionEvent): void {
		switch (event.type) {
			case 'timeline.item_created':
			case 'timeline.item_updated': {
				const item = parseTimelineItem(event.payload.item);
				if (item) this.upsertItem(item);
				break;
			}
			case 'timeline.snapshot': {
				this.items = sortItems(parseTimelineItems(event.payload.items));
				this.reconcileOptimistic();
				break;
			}
			case 'session.meta.updated': {
				const session = parseSession(event.payload.session);
				if (session) this.session = session;
				break;
			}
			case 'runtime.state.updated': {
				const state = parseRuntimeState(event.payload.state);
				if (state) this.runtimeState = state;
				break;
			}
			case 'runtime.notice.snapshot': {
				this.notices = parseNotices(event.payload.notices);
				break;
			}
			case 'runtime.notice.updated': {
				const notice = parseNotice(event.payload.notice);
				if (notice) this.upsertNotice(notice);
				break;
			}
			case 'session.subscribed': {
				const seq = cursorToSeq(asString(event.payload.eventCursor));
				if (seq !== null) this.cursorSeq = Math.max(this.cursorSeq, seq);
				break;
			}
			case 'session.refetch_required':
			case 'runtime.refetch_required': {
				this.scheduleRefetch();
				break;
			}
			default:
				break;
		}
		this.lastEventAt = Date.now();
		const seq = cursorToSeq(event.cursor) ?? (event.sequence >= 0 ? event.sequence : null);
		if (seq !== null) this.cursorSeq = Math.max(this.cursorSeq, seq);
	}

	private scheduleRefetch(): void {
		if (this.#refetchTimer !== null) return;
		this.#refetchTimer = setTimeout(() => {
			this.#refetchTimer = null;
			void this.refreshSnapshot().catch((error: unknown) => {
				this.error = describeError(error);
			});
		}, 120);
	}

	private upsertItem(item: TimelineItem): void {
		const next = [...this.items];
		const index = next.findIndex((existing) => existing.id === item.id);
		if (index >= 0) next[index] = item;
		else next.push(item);
		this.items = sortItems(next);
		if (item.source.clientMessageId) this.discardOptimistic(item.source.clientMessageId);
	}

	private reconcileOptimistic(): void {
		const known = new Set(
			this.items
				.map((item) => item.source.clientMessageId)
				.filter((value): value is string => value !== null && value.length > 0)
		);
		if (known.size === 0) return;
		this.optimistic = this.optimistic.filter((row) => !known.has(row.clientMessageId));
	}
}
