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

export function describeError(error: unknown): string {
	if (error instanceof Error) return error.message;
	if (typeof error === 'string') return error;
	return '未知错误';
}
