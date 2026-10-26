<script lang="ts">
	import { runtimeLabel } from '$lib/timeline';
	import { DSH_RUNTIME, availableRuntimes, dshGuidance } from '$lib/runtimes';
	import type { Connector } from '$lib/api/types';

	let {
		connector,
		mode = 'full'
	}: { connector: Connector; mode?: 'full' | 'dsh-only' } = $props();

	const runtimes = $derived(connector.runtimes);
	const available = $derived(availableRuntimes(connector));
	const guidance = $derived(dshGuidance(connector));
</script>

{#if mode === 'full'}
	<div class="panel">
		<p class="panel-title">运行时（由设备自己探测上报）</p>
		{#if runtimes.length === 0}
			<p class="muted-line">设备未上报可用运行时。</p>
		{:else}
			<ul>
				{#each runtimes as row (row.name)}
					<li class:off={!row.available}>
						<span class="name">{runtimeLabel(row.name)} · <code class="mono">{row.name}</code></span>
						<span class="state" data-tone={row.available ? 'ok' : 'warn'}>
							{row.available ? '可用' : '不可用'}
						</span>
						{#if !row.available}
							<span class="reason">{row.reason ?? '设备没有给出原因。'}</span>
						{/if}
					</li>
				{/each}
			</ul>
			{#if available.length === 0}
				<p class="warn-line">设备上报的运行时全部不可用，先修好设备侧再新建会话。</p>
			{/if}
		{/if}
	</div>
{/if}

{#if guidance}
	<div class="guide">
		<p class="guide-title">DSH 运行时不可用</p>
		<p>
			DSH 桥需要两端都开：设备侧 agent 必须带 <code class="mono">--runtime {DSH_RUNTIME}</code> 启动；DSH 侧要先在 DSH 里安装并启用 <code class="mono">@farwork/dsh-bridge</code> 插件（本仓 <code class="mono">packages/dsh-plugin</code>），桥才会在设备本地起来，agent 才探得到它。
		</p>
		{#if guidance.reason}
			<p>设备探测给出的原因（原文）：<span class="reason-raw">{guidance.reason}</span></p>
		{:else if !guidance.reported}
			<p class="muted-line">
				设备还没有上报 dsh 这一项（<code class="mono">runtimes</code> 里没有它），所以没有原因可以照抄；先确认 agent 已连上 hub 并带 <code class="mono">--runtime {DSH_RUNTIME}</code> 启动。
			</p>
		{:else}
			<p class="muted-line">设备把 dsh 报成不可用，但没有给出原因。</p>
		{/if}
	</div>
{/if}

<style>
	.panel,
	.guide {
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 12px 14px;
		border-radius: var(--radius-md);
		border: 1px solid var(--border);
		background: oklch(1 0 0 / 3%);
		font-size: 12.5px;
		line-height: 1.65;
	}

	.guide {
		border-color: color-mix(in oklch, var(--warn) 40%, transparent);
		background: color-mix(in oklch, var(--warn) 8%, transparent);
	}

	.panel-title {
		font-size: 11.5px;
		font-weight: 600;
		letter-spacing: 0.04em;
		color: var(--muted-foreground);
		text-transform: uppercase;
	}

	.guide-title {
		font-size: 12.5px;
		font-weight: 600;
		color: var(--warn);
	}

	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 4px;
	}

	li {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: 8px;
	}

	li.off .name {
		color: var(--muted-foreground);
	}

	.name {
		font-weight: 500;
	}

	.state {
		font-size: 11px;
		font-variant-numeric: tabular-nums;
	}

	.state[data-tone='ok'] {
		color: var(--ok);
	}

	.state[data-tone='warn'] {
		color: var(--warn);
	}

	.reason,
	.reason-raw {
		flex-basis: 100%;
		font-size: 11.5px;
		color: var(--muted-foreground);
	}

	.reason-raw {
		color: oklch(0.9 0.04 85);
	}

	.muted-line {
		font-size: 11.5px;
		color: var(--muted-foreground);
	}

	.warn-line {
		font-size: 11.5px;
		color: var(--warn);
	}

	code {
		font-size: 11.5px;
	}
</style>
