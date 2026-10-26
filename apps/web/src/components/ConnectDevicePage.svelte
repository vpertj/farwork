<script lang="ts">
	import { workspace } from '$lib/workspace.svelte';
	import { createConnector, listConnectors } from '$lib/api/workspace';
	import { describeError } from '$lib/format';
	import RuntimeAvailability from './RuntimeAvailability.svelte';
	import type { Connector } from '$lib/api/types';

	let { token }: { token: string } = $props();

	type Step = 'setup' | 'command' | 'connected' | 'timeout';
	type Runtime = 'codex' | 'dsh' | 'echo';

	/** agent 的编译期默认值：命令里与它相同的一律不写，少一个参数就少一次抄错。 */
	const AGENT_DEFAULT_HUB = 'ws://127.0.0.1:7300';
	const AGENT_DEFAULT_RUNTIME: Runtime = 'codex';
	const POLL_INTERVAL_MS = 2_000;
	const POLL_TIMEOUT_MS = 90_000;
	/** 仓库路径在浏览器里无从得知：命令里留占位符，由用户换成本机路径。 */
	const REPO_PLACEHOLDER = '/path/to/farwork';
	const COMMAND_VARIANTS: { value: 'binary' | 'source'; label: string }[] = [
		{ value: 'source', label: '从源码跑（默认）' },
		{ value: 'binary', label: '已编译（更快）' }
	];
	const RUNTIMES: { value: Runtime; label: string }[] = [
		{ value: 'codex', label: 'Codex' },
		{ value: 'dsh', label: 'dsh' },
		{ value: 'echo', label: 'echo' }
	];

	let step = $state<Step>('setup');
	let deviceName = $state('我的 Mac');
	let hubUrl = $state(defaultHubUrl());
	let workdir = $state('');
	let runtime = $state<Runtime>(AGENT_DEFAULT_RUNTIME);
	let variant = $state<'binary' | 'source'>('source');
	let busy = $state(false);
	let errorMessage = $state<string | null>(null);
	let pollError = $state<string | null>(null);
	let elapsedSeconds = $state(0);
	let createdToken = $state('');
	let createdConnectorId = $state('');
	let liveConnector = $state<Connector | null>(null);
	let copyState = $state<'idle' | 'done' | 'manual'>('idle');
	let commandField = $state<HTMLTextAreaElement | null>(null);

	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let pollGeneration = 0;
	let pollStartedAt = 0;

	// 静态产物由 hub 自己托管时 origin 就是 hub；dev server（5173/5199…）背后另有 hub，统一按 7300 推。
	// 这样默认值恰好等于 agent 的 --hub 默认，命令里可以整段省掉。
	// localhost 在 macOS 上先解析成 ::1，而 hub 只监听 IPv4，所以一律换成 127.0.0.1。
	function defaultHubUrl(): string {
		if (typeof window === 'undefined') return AGENT_DEFAULT_HUB;
		const raw = window.location.hostname;
		const host = raw === 'localhost' || raw.length === 0 ? '127.0.0.1' : raw;
		return window.location.protocol === 'https:' ? `wss://${host}` : `ws://${host}:7300`;
	}

	const effectiveHub = $derived(hubUrl.trim() || AGENT_DEFAULT_HUB);

	function agentArgs(): string {
		const parts = ['--token', createdToken];
		if (effectiveHub !== AGENT_DEFAULT_HUB) parts.push('--hub', effectiveHub);
		const dir = workdir.trim();
		if (dir.length > 0) parts.push('--workdir', dir);
		if (runtime !== AGENT_DEFAULT_RUNTIME) parts.push('--runtime', runtime);
		return parts.join(' ');
	}

	// 两条变体都能真跑：已编译的不依赖 cwd（cargo run 只在仓库根有效），从源码跑的必须先进仓库。
	const commands = $derived.by(() => {
		if (createdToken.length === 0) return { binary: '', source: '' };
		const args = agentArgs();
		return {
			binary: `${REPO_PLACEHOLDER}/target/debug/farwork-agent ${args}`,
			source: `cd ${REPO_PLACEHOLDER} && cargo run -p farwork-agent -- ${args}`
		};
	});

	const command = $derived(variant === 'binary' ? commands.binary : commands.source);

	function isAbsolutePath(value: string): boolean {
		return value.startsWith('/') || /^[A-Za-z]:[\/]/.test(value);
	}

	function stopPoll(): void {
		pollGeneration += 1;
		if (pollTimer !== null) {
			clearInterval(pollTimer);
			pollTimer = null;
		}
	}

	// 轮询只活在这个向导页里：它在 hub 侧看 status，浏览器的会话页一条请求都不多。
	function startPoll(connectorId: string): void {
		stopPoll();
		const generation = pollGeneration;
		pollStartedAt = Date.now();
		elapsedSeconds = 0;
		pollError = null;

		const tick = async (): Promise<void> => {
			if (generation !== pollGeneration) return;
			elapsedSeconds = Math.round((Date.now() - pollStartedAt) / 1000);
			try {
				const rows = await listConnectors(token);
				if (generation !== pollGeneration) return;
				pollError = null;
				const row = rows.find((item) => item.id === connectorId) ?? null;
				if (row) liveConnector = row;
				if (row && row.status === 'online') {
					stopPoll();
					step = 'connected';
					void workspace.load(token);
					return;
				}
			} catch (error) {
				if (generation !== pollGeneration) return;
				pollError = describeError(error);
			}
			if (generation !== pollGeneration) return;
			if (Date.now() - pollStartedAt >= POLL_TIMEOUT_MS) {
				stopPoll();
				step = 'timeout';
			}
		};

		void tick();
		pollTimer = setInterval(() => void tick(), POLL_INTERVAL_MS);
	}

	$effect(() => () => stopPoll());

	async function submitDevice(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		errorMessage = null;
		const hub = hubUrl.trim();
		if (hub.length > 0 && !/^wss?:\/\/.+/.test(hub)) {
			errorMessage = `hub 地址要形如 ${AGENT_DEFAULT_HUB}，留空则用 agent 默认值。`;
			return;
		}
		const dir = workdir.trim();
		if (dir.length > 0 && !isAbsolutePath(dir)) {
			errorMessage = '工作目录要填绝对路径（例如 /Users/you/work），或者留空用 agent 当前目录。';
			return;
		}
		busy = true;
		try {
			// 设备名留空就交给 hub 定（它有自己的兜底名），界面事后显示的是真实名字。
			const result = await createConnector(token, deviceName.trim());
			createdConnectorId = result.connector.id;
			createdToken = result.connectorToken;
			liveConnector = result.connector;
			step = 'command';
			copyState = 'idle';
			void workspace.load(token);
			startPoll(result.connector.id);
		} catch (error) {
			errorMessage = describeError(error);
		} finally {
			busy = false;
		}
	}

	function retryWait(): void {
		step = 'command';
		startPoll(createdConnectorId);
	}

	function restart(): void {
		stopPoll();
		step = 'setup';
		createdToken = '';
		createdConnectorId = '';
		liveConnector = null;
		pollError = null;
		copyState = 'idle';
		errorMessage = null;
	}

	function selectCommand(): void {
		commandField?.focus();
		commandField?.select();
	}

	async function copyCommand(): Promise<void> {
		try {
			await navigator.clipboard.writeText(command);
			copyState = 'done';
		} catch {
			// 非安全上下文或权限被拒时剪贴板不可用，退化为「已选中，手动复制」（同 ShareLinkCard）。
			copyState = 'manual';
			selectCommand();
		}
	}
</script>

<section class="connect" data-testid="connect-wizard">
	<header>
		<a class="back" href="#/" title="返回任务台">←</a>
		<div>
			<h1>连接设备</h1>
			<p>只需要一个 token，其余参数都有默认值。</p>
		</div>
	</header>

	{#if step !== 'setup'}
		<ol class="steps" aria-label="连接设备的步骤">
			<li class="done">1 · 生成命令</li>
			<li class:active={step === 'command' || step === 'timeout'} class:done={step === 'connected'}>
				2 · 等设备上线
			</li>
		</ol>
	{/if}

	{#if errorMessage}
		<p class="alert" role="alert">{errorMessage}</p>
	{/if}

	{#if step === 'setup'}
		<form onsubmit={submitDevice}>
			<label class="field">
				<span>设备名（可留空）</span>
				<input class="input" bind:value={deviceName} placeholder="我的 Mac" autocomplete="off" />
			</label>

			<div class="runtime">
				<p class="label">运行时</p>
				<div class="segmented" role="group" aria-label="运行时">
					{#each RUNTIMES as item (item.value)}
						<button
							class="seg"
							class:on={runtime === item.value}
							type="button"
							aria-pressed={runtime === item.value}
							onclick={() => (runtime = item.value)}
						>
							{item.label}
						</button>
					{/each}
				</div>
				<p class="hint">
					dsh 连本机 DSH 桥（要先在 DSH 里启用 agents-anywhere 插件）；echo 仅联调，不执行任何东西。
				</p>
			</div>

			<details class="advanced" data-testid="connect-advanced">
				<summary>高级选项 · 都有默认值，通常不用改</summary>
				<div class="advanced-body">
					<label class="field">
						<span>hub 地址</span>
						<input
							class="input mono"
							bind:value={hubUrl}
							spellcheck="false"
							autocomplete="off"
							placeholder={AGENT_DEFAULT_HUB}
						/>
					</label>
					<p class="hint">设备得能自己连到这个 ws(s) 地址，不一定是浏览器里的地址。</p>

					<label class="field">
						<span>工作目录</span>
						<input
							class="input mono"
							bind:value={workdir}
							placeholder="留空 = agent 当前目录"
							spellcheck="false"
							autocomplete="off"
						/>
					</label>
					<p class="hint">要填就必须是绝对路径：它是文件浏览与终端的沙箱根。</p>

				</div>
			</details>

			<button class="btn" data-variant="primary" type="submit" disabled={busy}>
				{busy ? '正在创建设备…' : '生成连接命令'}
			</button>
		</form>
	{:else if step === 'command'}
		<div class="card">
			<p class="card-title">在这台设备上执行这一条</p>
			<div class="segmented" role="group" aria-label="命令形式">
				{#each COMMAND_VARIANTS as item (item.value)}
					<button
						class="seg"
						class:on={variant === item.value}
						type="button"
						aria-pressed={variant === item.value}
						onclick={() => (variant = item.value)}
						data-testid={`connect-variant-${item.value}`}
					>
						{item.label}
					</button>
				{/each}
			</div>
			<textarea
				class="input mono command"
				data-testid="connect-command"
				readonly
				rows="3"
				bind:this={commandField}
				value={command}
			></textarea>
			<p class="hint">
				把 <code class="mono">{REPO_PLACEHOLDER}</code> 换成本仓库路径就行，其余不用改。
			</p>
			{#if workdir.trim().length === 0}
				<p class="hint">不填工作目录时用 agent 当前目录（也是文件浏览沙箱根）。</p>
			{/if}
			<div class="row">
				<button class="btn" data-variant="primary" type="button" onclick={() => void copyCommand()}>
					复制命令
				</button>
				<button class="btn" data-variant="ghost" type="button" onclick={restart}>重新开始</button>
			</div>
			{#if copyState === 'done'}
				<p class="hint ok-line">命令已复制到剪贴板。</p>
			{:else if copyState === 'manual'}
				<p class="hint warn-line">
					浏览器没有允许写入剪贴板，命令已选中：请按系统复制快捷键（⌘/Ctrl + C）或右键复制。
				</p>
			{/if}
			<p class="hint">token 只显示这一次（hub 只存 sha256），离开本页就取不回来。</p>
			<p class="hint">
				执行完不用点按钮：这里已经在自动检测，已等
				<strong data-testid="connect-elapsed">{elapsedSeconds}</strong> 秒。
			</p>
			{#if pollError}
				<p class="alert" role="alert">查询设备状态失败：{pollError}</p>
			{/if}
		</div>
	{:else if step === 'connected'}
		<div class="card ok-card">
			<p class="card-title ok-line">设备已连接</p>
			{#if liveConnector}
				<p>
					<strong>{liveConnector.name}</strong>
					<span class="badge" data-tone="ok">online</span>
					<span class="mono id">{liveConnector.id}</span>
				</p>
				<RuntimeAvailability connector={liveConnector} mode="full" />
			{/if}
			<div class="row">
				<a class="btn" data-variant="primary" href="#/new-session">去新建会话</a>
				<button class="btn" data-variant="ghost" type="button" onclick={restart}>再连一台设备</button>
			</div>
		</div>
	{:else}
		<div class="card warn-card">
			<p class="card-title warn-line">90 秒内没等到设备上线</p>
			<p>
				设备 <span class="mono">{createdConnectorId}</span> 仍是
				<strong>{liveConnector?.status ?? 'offline'}</strong>，hub 没收到它的连入。
			</p>
			{#if pollError}
				<p class="alert" role="alert">最后一次查询失败：{pollError}</p>
			{/if}
			<ol class="checklist">
				<li>
					最可能：token 已失效 —— hub 换过实例、重启或重建过数据库、设备被 revoke，或者 token 是从别的进程（例如占过 7300 的桩服务）拿的。
					动作：回上一步「重新建一台设备」，用新 token 重来。
				</li>
				<li>
					命令还在跑吗：回设备那个终端看 agent 是否已退出（缺 token、<code class="mono">--workdir</code>
					不是绝对路径都会以退出码 1 结束）。
				</li>
				<li>
					hub 地址通吗：<code class="mono">{effectiveHub}</code> 要能从设备上连到（hub 默认监听 7300）；
					别把 <code class="mono">127.0.0.1</code> 换成 <code class="mono">localhost</code>（macOS 先解析 ::1，hub 只监听 IPv4）。
				</li>
				<li>
					日志就在那个终端的 stdout/stderr；<code class="mono">$FARWORK_HOME/agent.json</code>
					只是身份文件（不含 token），删掉会换新的 agent_id。
				</li>
			</ol>
			<div class="row">
				<button class="btn" data-variant="primary" type="button" onclick={retryWait}>再等 90 秒</button>
				<button class="btn" data-variant="ghost" type="button" onclick={restart}>重新建一台设备</button>
			</div>
		</div>
	{/if}
</section>

<style>
	.connect {
		display: flex;
		flex-direction: column;
		gap: 14px;
		padding: 28px 32px;
		max-width: 760px;
		height: 100vh;
		overflow-y: auto;
	}

	header {
		display: flex;
		align-items: flex-start;
		gap: 10px;
	}

	.back {
		display: grid;
		place-items: center;
		width: 28px;
		height: 28px;
		border-radius: var(--radius-sm);
		color: var(--muted-foreground);
		flex: none;
	}

	.back:hover {
		background: oklch(1 0 0 / 8%);
		color: var(--foreground);
	}

	h1 {
		font-size: 20px;
		letter-spacing: -0.01em;
	}

	header p {
		margin-top: 6px;
		font-size: 12.5px;
		color: var(--muted-foreground);
		font-weight: 400;
		line-height: 1.6;
	}

	.steps {
		display: flex;
		gap: 6px;
		list-style: none;
		margin: 0;
		padding: 0;
		font-size: 11.5px;
		color: var(--muted-foreground);
	}

	.steps li {
		padding: 3px 10px;
		border-radius: 999px;
		border: 1px solid var(--border);
	}

	.steps li.active {
		border-color: color-mix(in oklch, var(--info) 45%, transparent);
		color: var(--info);
		background: color-mix(in oklch, var(--info) 12%, transparent);
	}

	.steps li.done {
		color: var(--ok);
		border-color: color-mix(in oklch, var(--ok) 35%, transparent);
	}

	form {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.advanced {
		border: 1px solid var(--border);
		border-radius: var(--radius-md);
		padding: 6px 12px 12px;
		font-size: 12.5px;
	}

	.advanced summary {
		cursor: pointer;
		padding: 4px 0;
		font-size: 12px;
		color: var(--muted-foreground);
	}

	.advanced summary:hover {
		color: var(--foreground);
	}

	.advanced-body {
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding-top: 8px;
	}

	.label {
		font-size: 12px;
		color: var(--muted-foreground);
	}

	.segmented {
		display: flex;
		gap: 6px;
		margin-top: 6px;
	}

	.seg {
		flex: 1;
		padding: 7px 10px;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border);
		background: transparent;
		color: var(--muted-foreground);
		font: inherit;
		font-size: 12px;
		cursor: pointer;
	}

	.seg:hover:not(.on) {
		color: var(--foreground);
	}

	.seg.on {
		border-color: color-mix(in oklch, var(--info) 45%, transparent);
		color: var(--info);
		background: color-mix(in oklch, var(--info) 12%, transparent);
	}

	.card {
		display: flex;
		flex-direction: column;
		gap: 10px;
		padding: 14px 16px;
		border-radius: var(--radius-md);
		border: 1px solid var(--border);
		background: oklch(1 0 0 / 3%);
		font-size: 12.5px;
		line-height: 1.65;
	}

	.ok-card {
		border-color: color-mix(in oklch, var(--ok) 40%, transparent);
		background: color-mix(in oklch, var(--ok) 8%, transparent);
	}

	.warn-card {
		border-color: color-mix(in oklch, var(--warn) 45%, transparent);
		background: color-mix(in oklch, var(--warn) 8%, transparent);
	}

	.card-title {
		font-size: 13px;
		font-weight: 600;
	}

	.command {
		height: auto;
		min-height: 64px;
		padding: 10px 12px;
		line-height: 1.6;
		resize: vertical;
		font-size: 11.5px;
		word-break: break-all;
	}

	.row {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}

	.hint {
		font-size: 11px;
		color: var(--muted-foreground);
	}

	.ok-line {
		color: var(--ok);
	}

	.warn-line {
		color: var(--warn);
	}

	.checklist {
		margin: 0;
		padding-left: 18px;
		display: flex;
		flex-direction: column;
		gap: 6px;
		font-size: 12px;
	}

	.id {
		font-size: 11px;
		color: var(--muted-foreground);
	}

	code {
		font-size: 11.5px;
	}
</style>
