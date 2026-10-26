<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { fetchHealth, getToken, TOKEN_STORAGE_KEY } from '$lib/api';
	import type { DeviceSummary } from '$lib/protocol';
	import { FarworkClient, wsUrl, type SocketState } from '$lib/ws';

	const PLATFORM_LABEL: Record<DeviceSummary['platform'], string> = {
		macos: 'macOS',
		linux: 'Linux',
		windows: 'Windows'
	};

	let socketState = $state<SocketState>('idle');
	let devices = $state<DeviceSummary[]>([]);
	let serverVersion = $state<string | null>(null);
	let tokenConfigured = $state(false);
	let now = $state(Date.now());

	const serverOnline = $derived(socketState === 'open');
	const connectionTone = $derived(
		socketState === 'open' ? 'ok' : socketState === 'closed' ? 'err' : 'warn'
	);
	const connectionLabel = $derived(
		socketState === 'open'
			? '服务已连接'
			: socketState === 'connecting'
				? '正在连接…'
				: socketState === 'reconnecting'
					? '连接中断，重连中…'
					: socketState === 'closed'
						? '服务未连接'
						: '等待连接'
	);
	const onlineCount = $derived(devices.filter((device) => device.connected).length);

	function isLive(device: DeviceSummary): boolean {
		return device.connected && serverOnline;
	}

	function formatRelative(timestamp: number, reference: number): string {
		const diff = Math.max(0, reference - timestamp);
		if (diff < 1000) return '刚刚';
		const seconds = Math.floor(diff / 1000);
		if (seconds < 60) return `${seconds} 秒前`;
		const minutes = Math.floor(seconds / 60);
		if (minutes < 60) return `${minutes} 分钟前`;
		const hours = Math.floor(minutes / 60);
		if (hours < 24) return `${hours} 小时前`;
		return new Date(timestamp).toLocaleString('zh-CN');
	}

	function openTerminal(device: DeviceSummary): void {
		if (!isLive(device)) return;
		void goto(`/terminal/${device.id}`);
	}

	onMount(() => {
		tokenConfigured = getToken() !== null;
		const clock = setInterval(() => {
			now = Date.now();
		}, 1000);

		void fetchHealth()
			.then((health) => {
				serverVersion = health.version;
			})
			.catch(() => {
				serverVersion = null;
			});

		const client = new FarworkClient(wsUrl('/ws/client'));
		const stopState = client.onState((state) => {
			socketState = state;
			if (state !== 'open') devices = [];
		});
		const stopDevices = client.onDevices((next) => {
			devices = next;
			now = Date.now();
		});
		client.connect();

		return () => {
			clearInterval(clock);
			stopState();
			stopDevices();
			client.close();
		};
	});
</script>

<svelte:head>
	<title>Farwork · 设备</title>
</svelte:head>

<div class="shell">
	<header>
		<div class="brand">
			<span class="glyph" aria-hidden="true">◈</span>
			<span class="wordmark">Farwork</span>
		</div>
		<div class="status">
			{#if serverVersion}
				<span class="version mono">v{serverVersion}</span>
			{/if}
			<span class="pill" data-tone={connectionTone}>{connectionLabel}</span>
		</div>
	</header>

	<main>
		<div class="headline">
			<h1>设备</h1>
			<span class="count">{onlineCount} / {devices.length} 在线</span>
		</div>

		<section class="list" aria-live="polite">
			{#if devices.length === 0}
				<div class="empty">
					{#if socketState === 'open'}
						<p class="empty-title">暂无设备</p>
						<p class="empty-body">启动 Connector 后，设备会自动出现在这里。</p>
					{:else if socketState === 'closed'}
						<p class="empty-title">无法连接远控服务</p>
						<p class="empty-body">确认 server 已在 7300 端口运行，随后刷新页面。</p>
					{:else}
						<p class="empty-title">正在连接远控服务…</p>
						<p class="empty-body">首次连接通常需要 1 秒。</p>
					{/if}
				</div>
			{:else}
				{#each devices as device (device.id)}
					<button
						class="card"
						type="button"
						disabled={!isLive(device)}
						onclick={() => openTerminal(device)}
					>
						<span class="dot" class:live={isLive(device)} aria-hidden="true"></span>
						<span class="identity">
							<span class="name">{device.name}</span>
							<span class="sub mono">{device.id.slice(0, 8)}</span>
						</span>
						<span class="tags">
							<span class="tag">{PLATFORM_LABEL[device.platform]}</span>
							<span class="tag" class:online={isLive(device)}>
								{isLive(device) ? '在线' : device.connected ? '服务离线' : '离线'}
							</span>
						</span>
						<span class="seen">最后活动 {formatRelative(device.last_seen_ms, now)}</span>
						<span class="enter" aria-hidden="true">→</span>
					</button>
				{/each}
			{/if}
		</section>

		<p class="hint">
			{#if tokenConfigured}
				已读取本地 Token（<code class="mono">localStorage.{TOKEN_STORAGE_KEY}</code>），请求会带上
				<code class="mono">Authorization: Bearer</code>。
			{:else}
				未设置 Token。若服务端配置了 <code class="mono">FARWORK_TOKEN</code>，请在浏览器控制台执行
				<code class="mono">localStorage.setItem('{TOKEN_STORAGE_KEY}', '&lt;你的 token&gt;')</code> 后刷新页面。
			{/if}
		</p>
	</main>
</div>

<style>
	.shell {
		min-height: 100dvh;
		display: flex;
		flex-direction: column;
		padding: 22px clamp(16px, 5vw, 40px) 40px;
		gap: 28px;
	}

	header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
	}

	.brand {
		display: flex;
		align-items: center;
		gap: 10px;
	}

	.glyph {
		color: var(--accent);
		font-size: 18px;
		line-height: 1;
	}

	.wordmark {
		font-weight: 600;
		letter-spacing: 0.02em;
	}

	.status {
		display: flex;
		align-items: center;
		gap: 10px;
	}

	.version {
		color: var(--dim);
		font-size: 12px;
	}

	main {
		flex: 1;
		width: min(760px, 100%);
		margin: 0 auto;
		display: flex;
		flex-direction: column;
		gap: 16px;
	}

	.headline {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 12px;
	}

	h1 {
		font-size: 26px;
		font-weight: 600;
		letter-spacing: -0.01em;
	}

	.count {
		color: var(--muted);
		font-size: 13px;
	}

	.list {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.card {
		display: grid;
		grid-template-columns: auto minmax(0, 1fr) auto;
		grid-template-areas:
			'dot identity tags'
			'dot seen enter';
		align-items: center;
		gap: 4px 14px;
		width: 100%;
		padding: 14px 16px;
		text-align: left;
		background: var(--panel);
		border: 1px solid var(--border);
		border-radius: var(--radius);
		cursor: pointer;
		transition:
			background 120ms ease,
			border-color 120ms ease,
			transform 120ms ease;
	}

	.card:hover:not(:disabled) {
		background: var(--panel-hover);
		border-color: var(--border-strong);
	}

	.card:active:not(:disabled) {
		transform: scale(0.995);
	}

	.card:disabled {
		cursor: not-allowed;
		opacity: 0.55;
	}

	.dot {
		grid-area: dot;
		width: 9px;
		height: 9px;
		border-radius: 50%;
		background: var(--dim);
	}

	.dot.live {
		background: var(--ok);
		box-shadow: 0 0 0 4px rgba(63, 185, 80, 0.14);
	}

	.identity {
		grid-area: identity;
		display: flex;
		align-items: baseline;
		gap: 10px;
		min-width: 0;
	}

	.name {
		font-weight: 550;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.sub {
		color: var(--dim);
		font-size: 11px;
		flex: none;
	}

	.tags {
		grid-area: tags;
		display: flex;
		gap: 6px;
	}

	.tag {
		padding: 2px 8px;
		border-radius: 6px;
		border: 1px solid var(--border);
		color: var(--muted);
		font-size: 11px;
		white-space: nowrap;
	}

	.tag.online {
		color: var(--text);
		border-color: rgba(63, 185, 80, 0.4);
	}

	.seen {
		grid-area: seen;
		color: var(--muted);
		font-size: 12px;
	}

	.enter {
		grid-area: enter;
		color: var(--dim);
		font-size: 14px;
	}

	.empty {
		padding: 36px 20px;
		border: 1px dashed var(--border);
		border-radius: var(--radius);
		text-align: center;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.empty-title {
		color: var(--text);
	}

	.empty-body {
		color: var(--muted);
		font-size: 13px;
	}

	.hint {
		margin-top: 4px;
		color: var(--muted);
		font-size: 12px;
		line-height: 1.7;
	}

	code {
		padding: 1px 5px;
		border-radius: 5px;
		background: var(--bg-soft);
		border: 1px solid var(--border);
		color: var(--text);
		font-size: 11px;
	}

	@media (max-width: 560px) {
		.card {
			grid-template-columns: auto minmax(0, 1fr);
			grid-template-areas:
				'dot identity'
				'dot tags'
				'dot seen';
		}

		.enter {
			display: none;
		}
	}
</style>
