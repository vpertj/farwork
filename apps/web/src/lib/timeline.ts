import type { TimelineItem } from './api/types';

const CLAUDE_INTERRUPTED_MARKERS = new Set([
	'[Request interrupted by user]',
	'[Request interrupted by user for tool use]'
]);

export function textOf(value: unknown): string | null {
	return typeof value === 'string' ? value : null;
}

export function messageText(item: TimelineItem): string {
	const content = item.content;
	return (
		textOf(content.text) ||
		textOf(content.content) ||
		textOf(content.message) ||
		textOf(content.rawText) ||
		''
	);
}

export function isVisibleItem(item: TimelineItem): boolean {
	if (item.type !== 'message' || item.source.runtime !== 'claude') return true;
	const text = messageText(item).trim();
	if (item.role === 'user' && CLAUDE_INTERRUPTED_MARKERS.has(text)) return false;
	return !(item.role === 'assistant' && text === 'No response requested.');
}

export function sortItems(items: readonly TimelineItem[]): TimelineItem[] {
	return [...items].sort(
		(a, b) => a.orderSeq - b.orderSeq || a.updatedSeq - b.updatedSeq || a.id.localeCompare(b.id)
	);
}

/** Runtime codes the hub accepts on create-and-start; labels come from `runtimeLabel`. */
export const SUPPORTED_RUNTIMES = ['codex', 'claude', 'opencode', 'acp', 'dsh'] as const;

export type SupportedRuntime = (typeof SUPPORTED_RUNTIMES)[number];

export function runtimeLabel(runtime: string): string {
	switch (runtime) {
		case 'codex':
			return 'Codex';
		case 'claude':
			return 'Claude Code';
		case 'opencode':
			return 'OpenCode';
		case 'acp':
			return 'ACP';
		case 'dsh':
			return 'DeepSeek Harness';
		case 'platform':
			return '平台';
		default:
			return runtime || '未知运行时';
	}
}

export type Tone = 'ok' | 'warn' | 'err' | 'info' | 'muted';

export function sessionStatusLabel(status: string): string {
	switch (status) {
		case 'idle':
			return '空闲';
		case 'waiting':
			return '等待输入';
		case 'pending':
			return '排队中';
		case 'running':
			return '运行中';
		case 'stopping':
			return '停止中';
		case 'waiting_approval':
			return '等待审批';
		case 'error':
			return '出错';
		case 'blocked':
			return '受阻';
		default:
			return status || '未知';
	}
}

export function sessionStatusTone(status: string): Tone {
	switch (status) {
		case 'running':
			return 'ok';
		case 'waiting':
		case 'waiting_approval':
		case 'pending':
			return 'warn';
		case 'error':
		case 'blocked':
			return 'err';
		default:
			return 'muted';
	}
}

export function itemStatusLabel(status: string): string {
	switch (status) {
		case 'pending':
			return '待处理';
		case 'running':
			return '进行中';
		case 'waiting_approval':
			return '等待审批';
		case 'done':
			return '完成';
		case 'failed':
			return '失败';
		case 'cancelled':
			return '已取消';
		case 'interrupted':
			return '已中断';
		default:
			return status;
	}
}

export function itemStatusTone(status: string): Tone {
	switch (status) {
		case 'running':
			return 'ok';
		case 'waiting_approval':
			return 'warn';
		case 'failed':
			return 'err';
		case 'done':
			return 'muted';
		default:
			return 'muted';
	}
}

export function itemTypeLabel(type: string): string {
	switch (type) {
		case 'message':
			return '消息';
		case 'tool':
			return '工具';
		case 'artifact':
			return '产物';
		case 'marker':
			return '标记';
		default:
			return '系统';
	}
}

function firstString(...values: unknown[]): string | null {
	for (const value of values) {
		const text = textOf(value);
		if (text && text.trim().length > 0) return text;
	}
	return null;
}

function stringifyBrief(value: unknown, limit = 400): string {
	try {
		const text = JSON.stringify(value, null, 1);
		if (typeof text !== 'string') return '';
		return text.length > limit ? `${text.slice(0, limit)}…` : text;
	} catch {
		return '';
	}
}

export function itemHeadline(item: TimelineItem): string {
	const content = item.content;
	const explicit = firstString(content.title, content.name, content.tool, content.toolName, content.kind);
	if (explicit) return explicit;
	if (item.type === 'message') return item.role === 'user' ? '用户' : '助手';
	if (item.type === 'tool') return '工具调用';
	if (item.type === 'artifact') return '产物';
	return itemTypeLabel(item.type);
}

export function itemBody(item: TimelineItem): string {
	const content = item.content;
	const text =
		item.type === 'message'
			? messageText(item)
			: firstString(content.summary, content.description, content.text, content.output, content.result) ??
				firstString(content.command) ??
				'';
	if (text) return text;
	const inline = firstString(content.command, content.arguments, content.input);
	if (inline) return inline;
	if (item.type === 'message') return '';
	return stringifyBrief(content);
}

export function isCollapsible(item: TimelineItem): boolean {
	return item.type === 'tool' || item.type === 'artifact';
}
