import { asRecord, asString, type NoticeOption, type RuntimeNotice } from './api/types';
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

/** 旧字符串形态只带 id，中文按既有口径补；未知 id 一律用服务端 label 或原样 id，绝不编造。 */
const LEGACY_ACTION_LABELS: Record<string, string> = {
	approve: '批准',
	approve_for_session: '本会话内总是批准',
	reject: '拒绝'
};

export function noticeOptionLabel(option: NoticeOption): string {
	return option.label || LEGACY_ACTION_LABELS[option.id] || option.id;
}

export function noticeActionLabel(notice: RuntimeNotice, actionId: string | null): string {
	if (!actionId) return '未给出决定';
	const option = notice.options.find((item) => item.id === actionId);
	return noticeOptionLabel(option ?? { id: actionId, label: '', style: null });
}

export function noticeDecisionLabel(notice: RuntimeNotice): string {
	return noticeActionLabel(notice, notice.decision);
}

export function noticeIsQuestion(notice: RuntimeNotice): boolean {
	return notice.interactionType === 'question';
}

/**
 * 只有服务端真的给了 schema 才渲染输入框：各 runtime 的字段形状不同，
 * 猜一个字段名等于给用户一个填了也没人看的框。
 */
export function noticeInputSchemaText(notice: RuntimeNotice): string | null {
	const body = notice.body;
	if (!body) return null;
	const schema = body.inputSchema ?? body.fields;
	if (schema === undefined || schema === null) return null;
	if (Array.isArray(schema)) {
		return schema.length > 0 ? clamp(JSON.stringify(schema, null, 1), INPUT_SCHEMA_LIMIT) : null;
	}
	const record = asRecord(schema);
	if (!record || Object.keys(record).length === 0) return null;
	return clamp(JSON.stringify(record, null, 1), INPUT_SCHEMA_LIMIT);
}

export type NoticeField = { label: string; value: string; mono: boolean };

const COMMAND_HEAD_LIMIT = 400;
const FILE_CHANGE_LIMIT = 8;
const INPUT_SCHEMA_LIMIT = 400;
const MESSAGE_LIMIT = 800;

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
export function noticeFields(notice: RuntimeNotice): NoticeField[] {
	const body = notice.body;
	if (!body) return [];
	const fields: NoticeField[] = [];
	const message = asString(body.message).trim();
	if (message) fields.push({ label: '消息', value: clamp(message, MESSAGE_LIMIT), mono: false });
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
		// 提问类没有可读文本就不铺原始 JSON：schema 已在输入区单独展示。
		if (noticeIsQuestion(notice)) return [];
		fields.push({ label: '原始内容', value: clamp(JSON.stringify(body, null, 1), 600), mono: true });
	}
	return fields;
}

export function noticeItemId(notice: RuntimeNotice): string | null {
	return notice.body ? asString(notice.body.itemId) || null : null;
}
