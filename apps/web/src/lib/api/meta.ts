import { apiRequest } from './client';
import { asNumber, asRecord, asString } from './types';

/** `GET /api/v2/meta`：hub 自报的对外基地址（免鉴权）。 */
export type HubMeta = {
	/** 形如 http://127.0.0.1:7300，无结尾斜杠。 */
	hubUrl: string;
	version: string;
	scheme: string;
	port: number;
};

/**
 * 配对命令里的地址只能来自这里，不能来自 `location.origin`：dev 下页面在 5173、hub
 * 在 7300（vite 代理），反代下两者更是两个域名。把页面 origin 写进 `--hub`，agent 会
 * 把它存进 agent.json，之后永远连不上，而报错只字不提病根。
 *
 * 返回 null 表示响应里没有可用地址：调用方回退到当前 origin，并在界面上标注。
 */
export async function fetchHubMeta(): Promise<HubMeta | null> {
	const payload = await apiRequest('/meta', { cache: 'no-store' });
	const raw = asRecord(payload);
	const hubUrl = asString(raw?.hubUrl)
		.trim()
		.replace(/\/+$/, '');
	if (!raw || !/^https?:\/\/.+/i.test(hubUrl)) return null;
	return {
		hubUrl,
		version: asString(raw.version),
		scheme: asString(raw.scheme),
		port: asNumber(raw.port)
	};
}
