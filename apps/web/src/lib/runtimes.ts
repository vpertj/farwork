import type { Connector, ConnectorRuntime } from './api/types';

/** DSH 桥是三层引导里最需要解释的一层：两端任一没开，设备就报不出可用的 dsh。 */
export const DSH_RUNTIME = 'dsh';

export function reportedRuntimes(connector: Connector | null): ConnectorRuntime[] {
	return connector ? connector.runtimes : [];
}

export function availableRuntimes(connector: Connector | null): ConnectorRuntime[] {
	return reportedRuntimes(connector).filter((row) => row.available);
}

export function runtimeEntry(connector: Connector | null, name: string): ConnectorRuntime | null {
	return reportedRuntimes(connector).find((row) => row.name === name) ?? null;
}

export type DshGuidance = {
	/** 设备探测报告的原因原文；没有上报时保持 null，界面不许替设备编一个。 */
	reason: string | null;
	/** 设备是否把 dsh 这一项报出来了（空 runtimes 表示没上报过）。 */
	reported: boolean;
};

/** 只有「需要引导」时才返回内容：dsh 可用就返回 null。 */
export function dshGuidance(connector: Connector | null): DshGuidance | null {
	if (!connector) return null;
	const entry = runtimeEntry(connector, DSH_RUNTIME);
	if (entry && entry.available) return null;
	return { reason: entry?.reason ?? null, reported: entry !== null };
}
