import type { TerminalClientFrame } from './api/types';

export type TerminalRequestFailure = {
	status: number;
	code: string | null;
	message: string;
};

export type TerminalFailureView = {
	title: string;
	hint: string;
	serverMessage: string;
	code: string | null;
	offline: boolean;
};

const BASE64_CHUNK = 0x2000;
const OFFLINE_CODE = /offline|disconnected|not[_ -]?connected|unreachable|device[_ -]?down/i;

export function inputFrame(text: string): TerminalClientFrame {
	return { type: 'terminal.input', data: encodeTextBase64(text) };
}

export function resizeFrame(cols: number, rows: number): TerminalClientFrame {
	return {
		type: 'terminal.resize',
		cols: Math.max(1, Math.trunc(cols)),
		rows: Math.max(1, Math.trunc(rows))
	};
}

export const CLOSE_FRAME: TerminalClientFrame = { type: 'terminal.close' };

export function decodeBase64Bytes(value: string): Uint8Array | null {
	const compact = value.replace(/\s+/g, '');
	if (compact.length === 0) return new Uint8Array(0);
	let binary: string;
	try {
		binary = atob(compact);
	} catch {
		return null;
	}
	const bytes = new Uint8Array(binary.length);
	for (let index = 0; index < binary.length; index += 1) {
		bytes[index] = binary.charCodeAt(index) & 0xff;
	}
	return bytes;
}

export function encodeBytesBase64(bytes: Uint8Array): string {
	let binary = '';
	for (let offset = 0; offset < bytes.length; offset += BASE64_CHUNK) {
		binary += String.fromCharCode(...bytes.subarray(offset, offset + BASE64_CHUNK));
	}
	return btoa(binary);
}

export function encodeTextBase64(text: string): string {
	return encodeBytesBase64(new TextEncoder().encode(text));
}

export function describeTerminalFailure(
	failure: TerminalRequestFailure | null,
	connectorOnline: boolean
): TerminalFailureView {
	const serverMessage = failure?.message ?? '';
	const code = failure?.code ?? null;
	if (!connectorOnline || (code !== null && OFFLINE_CODE.test(code))) {
		return {
			title: '设备不在线，无法打开终端',
			hint: '终端由设备上的 connector 起 PTY，设备离线时 hub 不能替你开一个。等设备重新上线后重试。',
			serverMessage,
			code,
			offline: true
		};
	}
	if (!failure) {
		return { title: '创建终端失败', hint: '未知原因。', serverMessage, code, offline: false };
	}
	switch (failure.status) {
		case 0:
			return {
				title: '创建终端的请求没有发出去',
				hint: '可能是网络断开或 hub 不可达，检查连接后重试。',
				serverMessage,
				code,
				offline: false
			};
		case 400:
		case 422:
			return {
				title: '服务端不接受这次创建终端的参数',
				hint: 'cols/rows/cwd 有服务端不认的值，见服务端消息。',
				serverMessage,
				code,
				offline: false
			};
		case 401:
		case 403:
			return {
				title: '没有权限在设备上开终端',
				hint: '登录状态可能过期，或这个 connector 不属于当前账号。',
				serverMessage,
				code,
				offline: false
			};
		case 404:
			return {
				title: '找不到终端接口或设备',
				hint: '这个 hub 可能还没实现终端接口，或 connector 已被撤销。',
				serverMessage,
				code,
				offline: false
			};
		case 501:
			return {
				title: '这个 hub 还没有实现终端接口',
				hint: '后端返回 501 not_implemented：终端属于未交付的路径，不是在假装工作。',
				serverMessage,
				code,
				offline: false
			};
		case 502:
			return {
				title: '设备侧起终端失败',
				hint: '设备上的 PTY 或 shell 启动不了，见服务端消息。',
				serverMessage,
				code,
				offline: false
			};
		case 503:
			return {
				title: '服务暂时不可用',
				hint: 'hub 此刻无法处理该请求，稍后重试。',
				serverMessage,
				code,
				offline: false
			};
		case 504:
			return {
				title: '设备响应超时',
				hint: '设备可能繁忙或网络抖动，重试一次。',
				serverMessage,
				code,
				offline: false
			};
		default:
			return {
				title: failure.status > 0 ? `创建终端失败（HTTP ${failure.status}）` : '创建终端失败',
				hint: '请求没有成功，原因见服务端消息。',
				serverMessage,
				code,
				offline: false
			};
	}
}

export function describeTerminalError(
	message: string,
	code: string | null,
	connectorOnline: boolean
): TerminalFailureView {
	if (code !== null && OFFLINE_CODE.test(code)) {
		return {
			title: '设备已断开，终端无法继续',
			hint: 'connector 掉线后 PTY 已不可用；设备重新上线后请新开终端。',
			serverMessage: message,
			code,
			offline: true
		};
	}
	if (code === 'terminal_not_found') {
		return {
			title: '终端不存在',
			hint: '服务端没有这个 terminalId，可能已超时回收或会话已结束。',
			serverMessage: message,
			code,
			offline: false
		};
	}
	return {
		title: code !== null && code.length > 0 ? `终端出错（${code}）` : '终端出错',
		hint: connectorOnline
			? '服务端终止了这个终端流，原因见消息。'
			: '设备不在线时终端流无法继续。',
		serverMessage: message,
		code,
		offline: !connectorOnline
	};
}
