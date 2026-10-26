export function formatRelative(iso: string | null, now = Date.now()): string {
	if (!iso) return '—';
	const timestamp = Date.parse(iso);
	if (Number.isNaN(timestamp)) return '—';
	const diff = Math.max(0, now - timestamp);
	if (diff < 60_000) return '刚刚';
	const minutes = Math.floor(diff / 60_000);
	if (minutes < 60) return `${minutes} 分钟前`;
	const hours = Math.floor(minutes / 60);
	if (hours < 24) return `${hours} 小时前`;
	const days = Math.floor(hours / 24);
	if (days < 30) return `${days} 天前`;
	return new Date(timestamp).toLocaleDateString('zh-CN');
}

export function formatClock(iso: string | null): string {
	if (!iso) return '';
	const timestamp = Date.parse(iso);
	if (Number.isNaN(timestamp)) return '';
	return new Date(timestamp).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
}

export function formatBytes(bytes: number): string {
	if (!Number.isFinite(bytes) || bytes < 0) return '—';
	if (bytes < 1024) return `${Math.trunc(bytes)} B`;
	const units = ['KB', 'MB', 'GB', 'TB'];
	let value = bytes / 1024;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	const digits = value < 10 ? 1 : 0;
	return `${value.toFixed(digits)} ${units[unit]}`;
}

export function formatModified(iso: string | null): string {
	if (!iso) return '—';
	const timestamp = Date.parse(iso);
	if (Number.isNaN(timestamp)) return '—';
	return new Date(timestamp).toLocaleString('zh-CN', {
		year: 'numeric',
		month: '2-digit',
		day: '2-digit',
		hour: '2-digit',
		minute: '2-digit'
	});
}

export function describeError(error: unknown): string {
	if (error instanceof Error) return error.message;
	if (typeof error === 'string') return error;
	return '未知错误';
}
