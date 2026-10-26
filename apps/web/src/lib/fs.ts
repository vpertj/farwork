import type { FsFailure } from './api/types';

export type FsFailureView = {
	title: string;
	hint: string;
	serverMessage: string;
	offline: boolean;
};

export type FsCrumb = { name: string; path: string };

const OFFLINE_CODE = /offline|disconnected|not[_ -]?connected|unreachable|device[_ -]?down/i;

export function trimFsPath(path: string): string {
	const trimmed = path.trim();
	if (trimmed.length <= 1) return trimmed;
	return trimmed.replace(/\/+$/, '') || '/';
}

export function joinFsPath(base: string, name: string): string {
	const parent = trimFsPath(base);
	if (parent === '') return name;
	return parent === '/' ? `/${name}` : `${parent}/${name}`;
}

export function fsBreadcrumb(root: string, current: string): FsCrumb[] {
	const rootPath = trimFsPath(root);
	if (rootPath === '') return [];
	const crumbs: FsCrumb[] = [{ name: rootPath, path: rootPath }];
	const currentPath = trimFsPath(current);
	if (currentPath === rootPath) return crumbs;
	const prefix = rootPath === '/' ? '/' : `${rootPath}/`;
	if (!currentPath.startsWith(prefix)) return crumbs;
	let walked = rootPath;
	for (const segment of currentPath.slice(prefix.length).split('/')) {
		if (segment.length === 0) continue;
		walked = joinFsPath(walked, segment);
		crumbs.push({ name: segment, path: walked });
	}
	return crumbs;
}

export function describeFsFailure(
	failure: FsFailure | null,
	connectorOnline: boolean
): FsFailureView {
	const serverMessage = failure?.message ?? '';
	if (!connectorOnline) {
		return {
			title: '设备不在线，无法读取文件',
			hint: '文件浏览走设备上的 connector，设备离线时 hub 不能替你读盘。等设备重新上线后重试。',
			serverMessage,
			offline: true
		};
	}
	if (!failure) {
		return { title: '读取失败', hint: '未知原因。', serverMessage, offline: false };
	}
	const code = failure.code ?? '';
	if (OFFLINE_CODE.test(code)) {
		return {
			title: '设备不在线，无法读取文件',
			hint: '连接器报告设备已断开。等设备重新上线后重试。',
			serverMessage,
			offline: true
		};
	}
	switch (failure.status) {
		case 0:
			return {
				title: '请求没有发出去',
				hint: '可能是网络断开或 hub 不可达，检查连接后重试。',
				serverMessage,
				offline: false
			};
		case 403:
			return {
				title: '没有权限读取这个位置',
				hint: '路径在工作目录之外，或设备上的账号读不了它。',
				serverMessage,
				offline: false
			};
		case 404:
			return {
				title: '路径不存在',
				hint: '它可能已被移动或删除，回到工作目录根再找一次。',
				serverMessage,
				offline: false
			};
		case 413:
			return {
				title: '文件过大，不做预览',
				hint: '超出一屏能诚实展示的范围，请到设备上用终端查看。',
				serverMessage,
				offline: false
			};
		case 502:
			return {
				title: '设备读取失败',
				hint: '设备侧磁盘或 IO 出错，稍后重试。',
				serverMessage,
				offline: false
			};
		case 504:
			return {
				title: '设备响应超时',
				hint: '设备可能繁忙或网络抖动，重试一次。',
				serverMessage,
				offline: false
			};
		case 503:
			return {
				title: '服务暂时不可用',
				hint: 'hub 此刻无法处理该请求，稍后重试。',
				serverMessage,
				offline: false
			};
		default:
			return {
				title: failure.status > 0 ? `读取失败（HTTP ${failure.status}）` : '读取失败',
				hint: '请求没有成功，原因见服务端消息。',
				serverMessage,
				offline: false
			};
	}
}
