export type Route =
	| { page: 'login' }
	| { page: 'register' }
	| { page: 'home' }
	| { page: 'session'; sessionId: string }
	| { page: 'settings'; tab: string }
	| { page: 'device'; connectorId: string }
	| { page: 'new-session'; projectId: string | null };

export function parseHash(hash: string): Route {
	const path = (hash.replace(/^#\/?/, '').split('?')[0] ?? '').trim();
	if (path === '') return { page: 'home' };
	const parts = path.split('/').filter((part) => part.length > 0);
	const head = parts[0] ?? '';
	switch (head) {
		case 'login':
			return { page: 'login' };
		case 'register':
			return { page: 'register' };
		case 'session':
			return parts[1] ? { page: 'session', sessionId: decodeURIComponent(parts[1]) } : { page: 'home' };
		case 'settings':
			return { page: 'settings', tab: parts[1] ?? 'account' };
		case 'device':
			return parts[1] ? { page: 'device', connectorId: decodeURIComponent(parts[1]) } : { page: 'home' };
		case 'new-session':
			return {
				page: 'new-session',
				projectId: parts[1] ? decodeURIComponent(parts[1]) : null
			};
		default:
			return { page: 'home' };
	}
}

export function hrefFor(route: Route): string {
	switch (route.page) {
		case 'login':
			return '#/login';
		case 'register':
			return '#/register';
		case 'home':
			return '#/';
		case 'session':
			return `#/session/${encodeURIComponent(route.sessionId)}`;
		case 'settings':
			return `#/settings/${encodeURIComponent(route.tab)}`;
		case 'device':
			return `#/device/${encodeURIComponent(route.connectorId)}`;
		case 'new-session':
			return route.projectId
				? `#/new-session/${encodeURIComponent(route.projectId)}`
				: '#/new-session';
	}
}

export function navigate(route: Route): void {
	const next = hrefFor(route);
	if (window.location.hash === next) return;
	window.location.hash = next;
}

export function isAuthRoute(route: Route): boolean {
	return route.page === 'login' || route.page === 'register';
}
