import { apiRequest } from './client';
import { parsePublicShare, parseSessionShare, type PublicShare, type SessionShare } from './types';

export type ShareInput = { scope: 'session' } | { scope: 'message'; messageId: string };

export async function createSessionShare(
	token: string,
	sessionId: string,
	input: ShareInput
): Promise<SessionShare> {
	const payload = await apiRequest(`/sessions/${encodeURIComponent(sessionId)}/shares`, {
		method: 'POST',
		token,
		body: input
	});
	const share = parseSessionShare(payload);
	if (!share) throw new Error('创建分享的响应格式无法识别。');
	return share;
}

// 公开分享页不带 token：这条路径必须能在未登录时成功，任何鉴权头都不允许附加。
export async function fetchPublicShare(shareId: string): Promise<PublicShare> {
	const payload = await apiRequest(`/public/shares/${encodeURIComponent(shareId)}`);
	const share = parsePublicShare(payload);
	if (!share) throw new Error('分享内容的响应格式无法识别。');
	return share;
}
