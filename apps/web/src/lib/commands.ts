import type { RuntimeCommand } from './api/types';

export const MAX_COMMAND_SUGGESTIONS = 8;

/** 只有整段草稿是「/ + 无空白的命令名」时才算在选命令；一旦出现空格就是普通文本。 */
export function slashQuery(draft: string): string | null {
	const text = draft.trimStart();
	if (!text.startsWith('/')) return null;
	if (/\s/.test(text)) return null;
	return text.slice(1);
}

export function matchCommands(commands: RuntimeCommand[], query: string): RuntimeCommand[] {
	const needle = query.toLowerCase();
	const seen = new Set<string>();
	const matched: RuntimeCommand[] = [];
	for (const command of commands) {
		const name = command.name.toLowerCase();
		if (needle.length > 0 && !name.startsWith(needle)) continue;
		if (seen.has(name)) continue;
		seen.add(name);
		matched.push(command);
		if (matched.length >= MAX_COMMAND_SUGGESTIONS) break;
	}
	return matched;
}

export function stepCommandIndex(index: number, delta: number, length: number): number {
	if (length <= 0) return -1;
	return (((index + delta) % length) + length) % length;
}

/** 选中命令只补全成普通文本，发不发送由用户按回车决定。 */
export function commandDraft(name: string): string {
	return `/${name} `;
}
