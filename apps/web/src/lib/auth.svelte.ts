import { ApiError } from './api/client';
import { fetchAuthConfig, fetchMe, loginWithPassword } from './api/auth';
import type { AuthConfig, AuthUser } from './api/types';
import { describeError } from './format';

const TOKEN_KEY = 'farwork.auth.token';

function readToken(): string | null {
	try {
		const value = window.localStorage.getItem(TOKEN_KEY);
		return value && value.length > 0 ? value : null;
	} catch {
		return null;
	}
}

class AuthStore {
	token = $state<string | null>(null);
	user = $state<AuthUser | null>(null);
	config = $state<AuthConfig | null>(null);
	ready = $state(false);
	busy = $state(false);
	error = $state<string | null>(null);

	get authenticated(): boolean {
		return this.token !== null;
	}

	async load(): Promise<void> {
		this.token = readToken();
		try {
			this.config = await fetchAuthConfig();
		} catch {
			this.config = null;
		}
		if (this.token) {
			try {
				this.user = await fetchMe(this.token);
			} catch (error) {
				if (error instanceof ApiError && (error.status === 401 || error.status === 403)) {
					this.clearToken();
					this.user = null;
				}
			}
		}
		this.ready = true;
	}

	async signIn(email: string, password: string): Promise<boolean> {
		this.busy = true;
		this.error = null;
		try {
			const session = await loginWithPassword(email, password);
			this.token = session.accessToken;
			this.user = session.user;
			try {
				window.localStorage.setItem(TOKEN_KEY, session.accessToken);
			} catch {
				// 存储不可用时仍保持内存态登录
			}
			return true;
		} catch (error) {
			this.error = describeError(error);
			return false;
		} finally {
			this.busy = false;
		}
	}

	signOut(): void {
		this.clearToken();
		this.user = null;
	}

	private clearToken(): void {
		this.token = null;
		try {
			window.localStorage.removeItem(TOKEN_KEY);
		} catch {
			// ignore
		}
	}
}

export const auth = new AuthStore();
