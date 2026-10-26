import { apiRequest } from './client';
import {
	asRecord,
	parseAdminSettings,
	parseAdminUser,
	parseAdminUserPage,
	type AdminRole,
	type AdminSettings,
	type AdminUser,
	type AdminUserPage
} from './types';

export type AdminUserQuery = { limit?: number; cursor?: string | null };

export async function listAdminUsers(
	token: string,
	query: AdminUserQuery = {}
): Promise<AdminUserPage> {
	const payload = await apiRequest('/admin/users', {
		token,
		query: { limit: query.limit, cursor: query.cursor ?? undefined }
	});
	const page = parseAdminUserPage(payload);
	if (!page) throw new Error('用户列表响应格式无法识别。');
	return page;
}

export type NewAdminUser = { email: string; password: string; role: AdminRole };

// 201 的响应体没有约定形状：能解析出用户就用它，解析不出返回 null，由调用方重拉列表。
export async function createAdminUser(token: string, input: NewAdminUser): Promise<AdminUser | null> {
	const payload = await apiRequest('/admin/users', {
		method: 'POST',
		token,
		body: { email: input.email.trim().toLowerCase(), password: input.password, role: input.role }
	});
	return parseAdminUser(asRecord(payload)?.user ?? payload);
}

export type AdminUserUpdate = { role?: AdminRole; disabled?: boolean; displayName?: string };

export async function updateAdminUser(
	token: string,
	userId: string,
	patch: AdminUserUpdate
): Promise<AdminUser | null> {
	const body: AdminUserUpdate = {};
	if (patch.role !== undefined) body.role = patch.role;
	if (patch.disabled !== undefined) body.disabled = patch.disabled;
	if (patch.displayName !== undefined) body.displayName = patch.displayName;
	const payload = await apiRequest(`/admin/users/${encodeURIComponent(userId)}`, {
		method: 'PATCH',
		token,
		body
	});
	return parseAdminUser(asRecord(payload)?.user ?? payload);
}

export async function fetchAdminSettings(token: string): Promise<AdminSettings> {
	const settings = parseAdminSettings(await apiRequest('/admin/settings', { token }));
	if (!settings) throw new Error('实例设置响应格式无法识别。');
	return settings;
}

export type AdminSettingsUpdate = {
	registrationOpen?: boolean;
	emailVerificationRequired?: boolean;
};

// 只有 PATCH 返回 200 才会走到这里；响应体解析不出设置时重读一次，避免用本地猜测冒充服务端状态。
export async function updateAdminSettings(
	token: string,
	patch: AdminSettingsUpdate
): Promise<AdminSettings> {
	const payload = await apiRequest('/admin/settings', { method: 'PATCH', token, body: patch });
	return parseAdminSettings(payload) ?? (await fetchAdminSettings(token));
}
