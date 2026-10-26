<script lang="ts">
	import { untrack } from 'svelte';
	import { Terminal } from '@xterm/xterm';
	import { FitAddon } from '@xterm/addon-fit';
	import '@xterm/xterm/css/xterm.css';
	import { parseTerminalFrame, type TerminalSession } from '$lib/api/types';
	import {
		createTerminalWsTicket,
		openTerminal,
		terminalFailure,
		terminalSocketUrl
	} from '$lib/api/workspace';
	import {
		CLOSE_FRAME,
		decodeBase64Bytes,
		describeTerminalError,
		describeTerminalFailure,
		inputFrame,
		resizeFrame,
		type TerminalFailureView
	} from '$lib/terminal';

	let {
		token,
		connectorId,
		cwd,
		connectorOnline
	}: {
		token: string;
		connectorId: string;
		cwd: string;
		connectorOnline: boolean;
	} = $props();

	type Phase = 'starting' | 'connecting' | 'live' | 'exited' | 'error' | 'disconnected';

	const RESIZE_DEBOUNCE_MS = 160;
	const FIT_DEBOUNCE_MS = 60;

	let phase = $state<Phase>('starting');
	let terminalId = $state('');
	let failure = $state<TerminalFailureView | null>(null);
	let exitCode = $state<number | null>(null);
	let dimensions = $state({ cols: 0, rows: 0 });
	let screenHost = $state<HTMLDivElement | null>(null);

	let term: Terminal | null = null;
	let fitAddon: FitAddon | null = null;
	let socket: WebSocket | null = null;
	let resizeTimer: ReturnType<typeof setTimeout> | null = null;
	let fitTimer: ReturnType<typeof setTimeout> | null = null;
	let observer: ResizeObserver | null = null;
	let generation = 0;
	let disposed = false;
	let booted = false;
	let createdDims = { cols: 80, rows: 24 };

	// 只把绝对路径当 cwd 传下去；相对的 workspacePath 交给设备用默认目录。
	const terminalCwd = $derived.by(() => {
		const value = cwd.trim();
		return value.startsWith('/') ? value : undefined;
	});

	const statusLabel = $derived(
		phase === 'live'
			? '已连接'
			: phase === 'exited'
				? '已退出'
				: phase === 'error'
					? '出错'
					: phase === 'disconnected'
						? '已断开'
						: '连接中'
	);
	const statusTone = $derived(
		phase === 'live' ? 'ok' : phase === 'error' ? 'err' : phase === 'starting' || phase === 'connecting' || phase === 'disconnected' ? 'warn' : ''
	);
	const phaseDetail = $derived(
		phase === 'starting'
			? '正在请求设备创建终端…'
			: phase === 'connecting'
				? '已创建终端，正在取 WS 票据并等待 terminal.opened…'
				: ''
	);
	const retryLabel = $derived(
		phase === 'exited' ? '重新打开' : phase === 'disconnected' ? '重新连接' : '重试'
	);
	const canRestart = $derived(
		phase === 'exited' || phase === 'disconnected' || phase === 'error'
	);
	const sizeLabel = $derived(dimensions.cols > 0 ? `${dimensions.cols}×${dimensions.rows}` : '—');

	$effect(() => {
		const host = screenHost;
		if (!host) return;
		const terminal = new Terminal({
			fontSize: 12,
			fontFamily: "ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, monospace",
			cursorBlink: true,
			scrollback: 5000,
			theme: {
				background: '#0b0b0c',
				foreground: '#e6e6e6',
				cursor: '#e6e6e6',
				selectionBackground: '#2f3a4d'
			}
		});
		const addon = new FitAddon();
		terminal.loadAddon(addon);
		terminal.open(host);
		term = terminal;
		fitAddon = addon;
		const inputSub = terminal.onData((data) => sendInput(data));
		const resizeSub = terminal.onResize(({ cols, rows }) => {
			dimensions = { cols, rows };
			scheduleResize(cols, rows);
		});
		observer = new ResizeObserver(() => scheduleFit());
		observer.observe(host);
		fitNow();
		if (!booted) {
			booted = true;
			untrack(() => {
				void start();
			});
		}
		return () => {
			disposed = true;
			generation += 1;
			inputSub.dispose();
			resizeSub.dispose();
			observer?.disconnect();
			observer = null;
			if (resizeTimer !== null) clearTimeout(resizeTimer);
			if (fitTimer !== null) clearTimeout(fitTimer);
			resizeTimer = null;
			fitTimer = null;
			closeTerminal();
			terminal.dispose();
			term = null;
			fitAddon = null;
		};
	});

	async function start(): Promise<void> {
		if (disposed) return;
		const run = (generation += 1);
		closeTerminal();
		phase = 'starting';
		failure = null;
		exitCode = null;
		terminalId = '';
		term?.reset();
		if (connectorId === '') {
			phase = 'error';
			failure = {
				title: '这个会话没有绑定设备',
				hint: '没有 connectorId 就无法在设备上创建 PTY。',
				serverMessage: '',
				code: null,
				offline: false
			};
			return;
		}
		let created: TerminalSession;
		try {
			created = await openTerminal(token, connectorId, {
				cwd: terminalCwd,
				cols: term?.cols ?? createdDims.cols,
				rows: term?.rows ?? createdDims.rows
			});
		} catch (error) {
			if (disposed || run !== generation) return;
			phase = 'error';
			failure = describeTerminalFailure(terminalFailure(error), connectorOnline);
			return;
		}
		if (disposed || run !== generation) return;
		terminalId = created.terminal.id;
		createdDims = { cols: created.terminal.cols, rows: created.terminal.rows };
		phase = 'connecting';
		let ticket: string;
		try {
			ticket = await createTerminalWsTicket(token, terminalId);
		} catch (error) {
			if (disposed || run !== generation) return;
			phase = 'error';
			failure = describeTerminalFailure(terminalFailure(error), connectorOnline);
			return;
		}
		if (disposed || run !== generation) return;
		const ws = new WebSocket(terminalSocketUrl(connectorId, terminalId, ticket));
		socket = ws;
		ws.onmessage = (event: MessageEvent<unknown>) => {
			if (disposed || run !== generation) return;
			handleFrame(event.data);
		};
		ws.onclose = () => {
			if (socket === ws) socket = null;
			if (disposed || run !== generation) return;
			if (phase === 'exited' || phase === 'error' || phase === 'disconnected') return;
			phase = 'disconnected';
		};
	}

	function handleFrame(raw: unknown): void {
		if (typeof raw !== 'string') return;
		let payload: unknown;
		try {
			payload = JSON.parse(raw) as unknown;
		} catch {
			return;
		}
		const frame = parseTerminalFrame(payload);
		if (!frame) return;
		switch (frame.type) {
			case 'terminal.opened':
				phase = 'live';
				term?.focus();
				if (term && (term.cols !== createdDims.cols || term.rows !== createdDims.rows)) {
					scheduleResize(term.cols, term.rows);
				}
				break;
			case 'terminal.data': {
				const bytes = decodeBase64Bytes(frame.data);
				// 字节直接交给 xterm：PTY 输出可能是任意字节或半个 UTF-8 序列。
				if (bytes && bytes.length > 0) term?.write(bytes);
				break;
			}
			case 'terminal.exit':
				exitCode = frame.code;
				phase = 'exited';
				shutdownSocket();
				break;
			case 'terminal.error':
				failure = describeTerminalError(frame.message, frame.code, connectorOnline);
				phase = 'error';
				shutdownSocket();
				break;
		}
	}

	function sendInput(data: string): void {
		const ws = socket;
		if (phase !== 'live' || !ws || ws.readyState !== WebSocket.OPEN) return;
		ws.send(JSON.stringify(inputFrame(data)));
	}

	function scheduleResize(cols: number, rows: number): void {
		if (resizeTimer !== null) clearTimeout(resizeTimer);
		resizeTimer = setTimeout(() => {
			resizeTimer = null;
			pushResize(cols, rows);
		}, RESIZE_DEBOUNCE_MS);
	}

	function pushResize(cols: number, rows: number): void {
		const ws = socket;
		if (phase !== 'live' || !ws || ws.readyState !== WebSocket.OPEN) return;
		ws.send(JSON.stringify(resizeFrame(cols, rows)));
	}

	function scheduleFit(): void {
		if (fitTimer !== null) clearTimeout(fitTimer);
		fitTimer = setTimeout(() => {
			fitTimer = null;
			fitNow();
		}, FIT_DEBOUNCE_MS);
	}

	function fitNow(): void {
		const host = screenHost;
		if (!host || !term || !fitAddon) return;
		if (host.clientWidth < 24 || host.clientHeight < 24) return;
		try {
			fitAddon.fit();
		} catch {
			return;
		}
		dimensions = { cols: term.cols, rows: term.rows };
	}

	function shutdownSocket(): void {
		const ws = socket;
		socket = null;
		if (!ws) return;
		ws.onmessage = null;
		ws.onclose = null;
		ws.onerror = null;
		if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) ws.close();
	}

	function closeTerminal(): void {
		const ws = socket;
		if (ws && ws.readyState === WebSocket.OPEN && (phase === 'live' || phase === 'connecting')) {
			ws.send(JSON.stringify(CLOSE_FRAME));
		}
		shutdownSocket();
	}

	function restart(): void {
		if (disposed) return;
		void start();
	}
</script>

<aside class="terminal-panel" aria-label="终端">
	<header class="panel-head">
		<span class="panel-title">终端</span>
		<span class="badge" data-tone={statusTone}>{statusLabel}</span>
		<span class="dims mono" title="当前终端行列数">{sizeLabel}</span>
		{#if canRestart}
			<button class="btn" type="button" onclick={restart}>{retryLabel}</button>
		{/if}
	</header>

	<p class="meta mono" title={terminalId || '尚未创建终端'}>
		{terminalId ? `terminalId ${terminalId}` : '尚未创建终端'}
	</p>
	<p class="meta" title={terminalCwd ?? '未指定工作目录'}>
		工作目录：{terminalCwd ?? '未指定（由设备决定）'}
	</p>

	{#if phaseDetail}
		<p class="note" role="status">连接中 · {phaseDetail}</p>
	{/if}

	{#if failure}
		<div class="panel-error" role="alert">
			<p class="err-title">{failure.title}</p>
			<p class="err-hint">{failure.hint}</p>
			{#if failure.serverMessage}
				<p class="server-msg mono">服务端：{failure.serverMessage}</p>
			{/if}
			{#if failure.code}
				<p class="server-msg mono">错误码：{failure.code}</p>
			{/if}
		</div>
	{/if}

	{#if phase === 'exited'}
		<div class="panel-exit" role="status">
			<p class="exit-title">进程已退出（code={exitCode ?? '未知'}）</p>
			<p class="err-hint">
				PTY 已结束，输出停在这里。「重新打开」会在设备上新建终端（新的 terminalId）。
			</p>
		</div>
	{/if}

	{#if phase === 'disconnected'}
		<div class="panel-error" role="alert">
			<p class="err-title">WebSocket 已断开</p>
			<p class="err-hint">
				终端流中断，屏幕上保留的是断开前收到的内容。「重新连接」会新开一个终端（新的
				terminalId），旧会话不会恢复。
			</p>
		</div>
	{/if}

	<div class="screen" bind:this={screenHost} data-phase={phase}></div>

	<footer class="foot">
		<span class="hint">
			设备上的真实 PTY：输出按 base64 → Uint8Array 直接写入，不做字符串解码；输入编码成 base64 上行。
		</span>
	</footer>
</aside>

<style>
	.terminal-panel {
		display: flex;
		flex-direction: column;
		width: 560px;
		flex: none;
		min-height: 0;
		height: 100vh;
		border-left: 1px solid var(--border);
		background: oklch(0.17 0 0);
	}

	@media (max-width: 1200px) {
		.terminal-panel {
			width: min(460px, 48vw);
		}
	}

	.panel-head {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 12px 14px;
		border-bottom: 1px solid var(--border);
	}

	.panel-title {
		font-size: 13px;
		font-weight: 600;
		flex: 1;
	}

	.dims {
		font-size: 10.5px;
		color: var(--muted-foreground);
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}

	.meta {
		padding: 6px 14px 0;
		font-size: 10.5px;
		color: var(--muted-foreground);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.note {
		margin: 8px 14px 0;
		padding: 6px 10px;
		border-radius: var(--radius-md);
		border: 1px solid color-mix(in oklch, var(--warn) 40%, transparent);
		background: color-mix(in oklch, var(--warn) 10%, transparent);
		color: oklch(0.9 0.06 85);
		font-size: 11px;
	}

	.panel-error,
	.panel-exit {
		display: flex;
		flex-direction: column;
		gap: 6px;
		margin: 8px 14px 0;
		padding: 10px 12px;
		border-radius: var(--radius-md);
	}

	.panel-error {
		border: 1px solid color-mix(in oklch, var(--err) 42%, transparent);
		background: color-mix(in oklch, var(--err) 10%, transparent);
	}

	.panel-exit {
		border: 1px solid color-mix(in oklch, var(--warn) 40%, transparent);
		background: color-mix(in oklch, var(--warn) 8%, transparent);
	}

	.err-title,
	.exit-title {
		font-size: 12px;
		font-weight: 600;
	}

	.err-title {
		color: oklch(0.86 0.09 22);
	}

	.exit-title {
		color: var(--warn);
	}

	.err-hint {
		font-size: 11px;
		color: var(--muted-foreground);
		line-height: 1.6;
	}

	.server-msg {
		font-size: 10.5px;
		color: oklch(0.86 0.06 22);
		word-break: break-word;
	}

	.screen {
		flex: 1;
		min-height: 0;
		margin-top: 8px;
		padding: 6px 0 6px 8px;
		overflow: hidden;
	}

	.screen :global(.xterm) {
		height: 100%;
	}

	.foot {
		padding: 8px 14px 12px;
		border-top: 1px solid var(--border);
	}

	.hint {
		font-size: 10.5px;
		color: var(--muted-foreground);
		line-height: 1.6;
	}
</style>
