import { apiRequest } from './client';
import { asRecord, asString, parseAuthConfig, parseAuthUser, type AuthConfig, type AuthUser } from './types';
import { derivePasswordVerifier } from './verifier';

export async function fetchAuthConfig(): Promise<AuthConfig> {
	const config = parseAuthConfig(await apiRequest('/auth/config'));
	if (!config) throw new Error('认证配置响应格式无法识别。');
	return config;
}

export async function requestPasswordSalt(email: string): Promise<string> {
	const payload = await apiRequest('/auth/password-salt', {
		method: 'POST',
		body: { email: email.trim().toLowerCase() }
	});
	return asString(asRecord(payload)?.salt);
}

export type AuthSession = { accessToken: string; user: AuthUser };

export async function loginWithPassword(email: string, password: string): Promise<AuthSession> {
	const normalizedEmail = email.trim().toLowerCase();
	const salt = await requestPasswordSalt(normalizedEmail);
	if (!salt) throw new Error('服务器未返回密码盐，无法登录。');
	const passwordVerifier = await derivePasswordVerifier(password, salt);
	const payload = await apiRequest('/auth/login', {
		method: 'POST',
		body: { email: normalizedEmail, passwordVerifier }
	});
	const record = asRecord(payload) ?? {};
	const accessToken = asString(record.accessToken);
	if (!accessToken) throw new Error('服务器未返回访问令牌。');
	const user = parseAuthUser(record) ?? {
		userId: asString(record.userId, 'unknown'),
		email: normalizedEmail,
		displayName: asString(record.displayName, normalizedEmail),
		role: asString(record.role, 'member'),
		disabled: false
	};
	return { accessToken, user };
}

export async function fetchMe(token: string): Promise<AuthUser | null> {
	return parseAuthUser(await apiRequest('/auth/me', { token }));
}
