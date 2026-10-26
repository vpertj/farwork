import { apiRequest } from './client';
import { asRecord, asString } from './types';

export type PairingStatus = 'pending' | 'claimed' | 'expired' | 'unknown';

export type PairingCode = {
	id: string;
	/** 服务端生成的一次性码（形如 4F2K-9QXM），只在生成它的那一次响应里出现。 */
	code: string;
	expiresAt: string;
};

export type PairingSnapshot = {
	id: string;
	status: PairingStatus;
	connectorId: string | null;
	expiresAt: string | null;
};

export type StartedPairing = { pairing: PairingCode; serverNowMs: number };
export type PolledPairing = { pairing: PairingSnapshot; serverNowMs: number };

const STATUSES: PairingStatus[] = ['pending', 'claimed', 'expired', 'unknown'];

function parseStatus(value: unknown): PairingStatus {
	const raw = asString(value).trim().toLowerCase();
	const match = STATUSES.find((status) => status === raw);
	// 认不出的状态按 unknown 处理：宁可让用户重新生成，也不要显示一个我们理解不了的状态。
	return match ?? 'unknown';
}

// serverTime 与本机时钟的差值只影响倒计时显示；过期判定仍以服务端 expiresAt 为准。
function readServerNowMs(raw: Record<string, unknown>): number {
	const parsed = Date.parse(asString(raw.serverTime));
	return Number.isNaN(parsed) ? Date.now() : parsed;
}

export async function startPairing(token: string): Promise<StartedPairing> {
	const payload = await apiRequest('/pairing/start', {
		method: 'POST',
		token,
		body: {},
		cache: 'no-store'
	});
	const raw = asRecord(payload);
	const pairing = asRecord(raw?.pairing);
	const id = asString(pairing?.id);
	const code = asString(pairing?.code);
	const expiresAt = asString(pairing?.expiresAt);
	if (!raw || !pairing || id.length === 0 || code.length === 0 || expiresAt.length === 0) {
		throw new Error('生成配对码的响应无法识别：缺 id/code/expiresAt。');
	}
	return { pairing: { id, code, expiresAt }, serverNowMs: readServerNowMs(raw) };
}

export async function fetchPairing(token: string, pairingId: string): Promise<PolledPairing> {
	const payload = await apiRequest(`/pairing/${encodeURIComponent(pairingId)}`, {
		token,
		cache: 'no-store'
	});
	const raw = asRecord(payload);
	const pairing = asRecord(raw?.pairing);
	if (!raw || !pairing) throw new Error('配对状态的响应无法识别。');
	const id = asString(pairing.id, pairingId);
	const connectorId = asString(pairing.connectorId);
	const expiresAt = asString(pairing.expiresAt);
	return {
		pairing: {
			id: id.length > 0 ? id : pairingId,
			status: parseStatus(pairing.status),
			connectorId: connectorId.length > 0 ? connectorId : null,
			expiresAt: expiresAt.length > 0 ? expiresAt : null
		},
		serverNowMs: readServerNowMs(raw)
	};
}