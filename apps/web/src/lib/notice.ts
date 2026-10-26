import { asRecord, asString, type RuntimeNotice } from './api/types';
import type { Tone } from './timeline';

export function noticeKindLabel(kind: string): string {
	switch (kind) {
		case 'command_execution':
			return '命令执行';
		case 'file_change':
			return '文件变更';
		case 'permissions':
			return '权限申请';
		default:
			return kind;
	}
}

export function noticeKindTone(kind: string): Tone {
	switch (kind) {
		case 'command_execution':
			return 'warn';
		case 'file_change':
			return 'info';
		case 'permissions':
			return 'err';
		default:
			return 'muted';
	}
}

export function noticeStatusLabel(notice: RuntimeNotice): string {
	if (notice.status === 'resolved') return '已处理';
	if (notice.status === 'open') return '待审批';
	return notice.status;
}

export function noticeDecisionLabel(decision: string | null): string {
	switch (decision) {
		case 'approve':
			return '已批准（本次）';
		case 'approve_for_session':
			return '已批准（本会话内总是）';
		case 'reject':
			return '已拒绝';
		default:
			return decision ?? '未给出决定';
	}
}

export type NoticeField = { label: string; value: string; mono: boolean };

const COMMAND_HEAD_LIMIT = 400;
const FILE_CHANGE_LIMIT = 8;

function commandText(value: unknown): string | null {
	if (typeof value === 'string') return value.trim().length > 0 ? value.trim() : null;
	if (!Array.isArray(value)) return null;
	const parts = value.filter((part): part is string => typeof part === 'string');
	const joined = parts.join(' ').trim();
	return joined.length > 0 ? joined : null;
}

function clamp(text: string, limit: number): string {
	return text.length > limit ? `${text.slice(0, limit)}…` : text;
}

function fileChangeText(value: unknown): string | null {
	const record = asRecord(value);
	if (!record) return null;
	const entries = Object.entries(record);
	if (entries.length === 0) return null;
	const lines = entries.slice(0, FILE_CHANGE_LIMIT).map(([path, change]) => {
		const detail = asRecord(change);
		const kind = detail ? asString(detail.type) || asString(detail.kind) : '';
		return kind ? `${path}（${kind}）` : path;
	});
	const rest = entries.length - lines.length;
	const suffix = rest > 0 ? `\n…另有 ${rest} 个文件` : '';
	return `${entries.length} 个文件\n${lines.join('\n')}${suffix}`;
}

/**
 * Reads the fields the runtime actually put in `notice.body`; every kind has its
 * own shape (see farwork-agent codex mapper), so nothing is inferred per-kind —
 * a body with no known field falls back to the raw JSON instead of a guess.
 */
export function noticeFields(body: Record<string, unknown> | null): NoticeField[] {
	if (!body) return [];
	const fields: NoticeField[] = [];
	const command = commandText(body.command);
	if (command) fields.push({ label: '命令', value: clamp(command, COMMAND_HEAD_LIMIT), mono: true });
	const cwd = asString(body.cwd);
	if (cwd) fields.push({ label: '工作目录', value: cwd, mono: true });
	const reason = asString(body.reason);
	if (reason) fields.push({ label: '原因', value: reason, mono: false });
	const grantRoot = asString(body.grantRoot);
	if (grantRoot) fields.push({ label: '授权根目录', value: grantRoot, mono: true });
	const changes = fileChangeText(body.fileChanges);
	if (changes) fields.push({ label: '变更文件', value: changes, mono: true });
	if (fields.length === 0) {
		fields.push({ label: '原始内容', value: clamp(JSON.stringify(body, null, 1), 600), mono: true });
	}
	return fields;
}

export function noticeItemId(notice: RuntimeNotice): string | null {
	return notice.body ? asString(notice.body.itemId) || null : null;
}
