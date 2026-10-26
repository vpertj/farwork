<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { Terminal } from '@xterm/xterm';
	import { FitAddon } from '@xterm/addon-fit';
	import { WebLinksAddon } from '@xterm/addon-web-links';
	import '@xterm/xterm/css/xterm.css';
	import { fetchDevices } from '$lib/api';
	import type { DeviceSummary } from '$lib/protocol';
	import { FarworkClient, wsUrl, type SocketState, type TerminalHandle } from '$lib/ws';

	type StreamStatus = 'connecting' | 'attached' | 'disconnected' | 'error' | 'exited';

	const deviceId = $derived(page.params.deviceId ?? '');

	let status = $state<StreamStatus>('connecting');
	let detail = $state('');
	let socketState = $state<SocketState>('idle');
	let device = $state<DeviceSummary | null>(null);
	let host = $state<HTMLDivElement>();

	let terminal: Terminal | null = null;
	let handle: TerminalHandle | null = null;
	let client: FarworkClient | null = null;

	const statusLabel = $derived(
		status === 'attached'
			? '已连接'
			: status === 'connecting'
				? '正在打开终端…'
				: status === 'disconnected'
					? '连接中断'
					: status === 'exited'
						? '会话已结束'
						: '出错了'
	);
	const statusTone = $derived(status === 'attached' ? 'ok' : status === 'connecting' ? 'warn' : 'err');
	const restorable = $derived(status !== 'attached' && status !== 'connecting');

	function attach(): void {
		if (!client || !terminal || handle || !client.isOpen()) return;
		terminal.reset();
		detail = '';
		status = 'connecting';
		handle = client.openTerminal(deviceId, terminal.cols, terminal.rows, {
			onOpened: () => {
				status = 'attached';
				terminal?.focus();
			},
			onData: (bytes) => {
				terminal?.write(bytes);
			},
			onError: (message) => {
				status = 'error';
				detail = message;
				handle = null;
			},
			onExit: (code) => {
				status = 'exited';
				detail = code === 0 ? '远端已结束该终端会话' : `远端结束会话，退出码 ${code}`;
				handle = null;
			},
			onDisconnected: () => {
				status = 'disconnected';
				detail = '与远控服务的连接已断开，正在重连…';
				handle = null;
			}
		});
	}

	function retry(): void {
		if (!client) return;
		if (client.isOpen()) {
			attach();
			return;
		}
		client.reconnect();
	}

	onMount(() => {
		const term = new Terminal({
			cursorBlink: true,
			fontSize: 13,
			fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
			scrollback: 5000,
			allowProposedApi: false,
			theme: {
				background: '#0b0d10',
				foreground: '#e6edf3',
				cursor: '#4cc2ff',
				cursorAccent: '#0b0d10',
				selectionBackground: 'rgba(76, 194, 255, 0.28)',
				black: '#1b222b',
				red: '#f85149',
				green: '#3fb950',
				yellow: '#d29922',
				blue: '#4cc2ff',
				magenta: '#bc8cff',
				cyan: '#39c5cf',
				white: '#b1bac4',
				brightBlack: '#6e7681',
				brightRed: '#ff7b72',
				brightGreen: '#56d364',
				brightYellow: '#e3b341',
				brightBlue: '#79c0ff',
				brightMagenta: '#d2a8ff',
				brightCyan: '#56d4dd',
				brightWhite: '#f0f6fc'
			}
		});
		const fit = new FitAddon();
		term.loadAddon(fit);
		term.loadAddon(new WebLinksAddon());
		if (host) term.open(host);
		terminal = term;

		const fitNow = () => {
			try {
				fit.fit();
			} catch {
				return;
			}
		};
		fitNow();

		const onData = term.onData((data) => {
			handle?.write(data);
		});
		const onResize = term.onResize(({ cols, rows }) => {
			handle?.resize(cols, rows);
		});

		const observer = new ResizeObserver(() => fitNow());
		if (host) observer.observe(host);

		void fetchDevices()
			.then((list) => {
				device = list.find((item) => item.id === deviceId) ?? null;
			})
			.catch(() => undefined);

		client = new FarworkClient(wsUrl('/ws/client'));
		const stopState = client.onState((state) => {
			socketState = state;
			if (state === 'open') attach();
		});
		client.connect();

		return () => {
			observer.disconnect();
			onData.dispose();
			onResize.dispose();
			stopState();
			client?.close();
			client = null;
			handle = null;
			term.dispose();
			terminal = null;
		};
	});
</script>

<svelte:head>
	<title>Farwork · 终端</title>
</svelte:head>

<div class="page">
	<header>
		<a class="back" href="/">← 设备</a>
		<div class="title">
			<span class="name">{device?.name ?? deviceId.slice(0, 8)}</span>
			<span class="id mono">{deviceId}</span>
		</div>
		<div class="controls">
			<span class="pill" data-tone={statusTone}>{statusLabel}</span>
			{#if socketState !== 'open'}
				<span class="socket">服务器 {socketState === 'closed' ? '已断开' : '重连中'}</span>
			{/if}
			{#if restorable}
				<button type="button" class="retry" onclick={retry}>重新连接</button>
			{/if}
		</div>
	</header>

	{#if detail}
		<p class="detail">{detail}</p>
	{/if}

	<div class="terminal" bind:this={host}></div>
</div>

<style>
	.page {
		height: 100dvh;
		display: flex;
		flex-direction: column;
		background: var(--bg);
	}

	header {
		display: flex;
		align-items: center;
		gap: 14px;
		padding: 10px clamp(12px, 3vw, 20px);
		border-bottom: 1px solid var(--border);
		background: var(--bg-soft);
	}

	.back {
		font-size: 13px;
		color: var(--muted);
	}

	.back:hover {
		color: var(--accent);
	}

	.title {
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

	.id {
		color: var(--dim);
		font-size: 11px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.controls {
		margin-left: auto;
		display: flex;
		align-items: center;
		gap: 10px;
	}

	.socket {
		color: var(--err);
		font-size: 12px;
	}

	.retry {
		padding: 5px 12px;
		border-radius: 8px;
		border: 1px solid var(--border-strong);
		background: var(--panel);
		color: var(--text);
		font-size: 12px;
		cursor: pointer;
	}

	.retry:hover {
		background: var(--panel-hover);
	}

	.detail {
		padding: 8px clamp(12px, 3vw, 20px);
		color: var(--warn);
		font-size: 12px;
		border-bottom: 1px solid var(--border);
	}

	.terminal {
		flex: 1;
		min-height: 0;
		padding: 8px 10px;
		overflow: hidden;
	}

	.terminal :global(.xterm) {
		height: 100%;
	}
</style>
