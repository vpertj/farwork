<script lang="ts">
	import { workspace } from '$lib/workspace.svelte';
	import { createConnector, listConnectors } from '$lib/api/workspace';
	import { describeError } from '$lib/format';
	import RuntimeAvailability from './RuntimeAvailability.svelte';
	import type { Connector } from '$lib/api/types';

	let { token }: { token: string } = $props();

	type Step = 'name' | 'command' | 'waiting' | 'connected' | 'timeout';

	const POLL_INTERVAL_MS = 2_000;
	const POLL_TIMEOUT_MS = 90_000;

	let step = $state<Step>('name');
	let deviceName = $state('');
	let hubUrl = $state(defaultHubUrl());
	let workdir = $state('');
	let runtime = $state<'codex' | 'dsh'>('dsh');
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

	// 5173 是 vite dev（hub 在后面另一个端口），其它情况按浏览器自己的 origin 推；两种都允许手改。
	function defaultHubUrl(): string {
		if (typeof window === 'undefined') return 'ws://127.0.0.1:7300';
		const { protocol, hostname, port } = window.location;
		const host = hostname || '127.0.0.1';
		if (port === '5173') return `ws://${host}:7300`;
		const scheme = protocol === 'https:' ? 'wss:' : 'ws:';
		return `${scheme}//${host}${port ? `:${port}` : ''}`;
	}

	const command = $derived(
		createdToken.length === 0
			? ''
			: `cargo run -p farwork-agent -- --token ${createdToken} --hub ${hubUrl.trim()} --workdir ${workdir.trim()} --runtime ${runtime}`
	);

	function isAbsolutePath(value: string): boolean {
		return value.startsWith('/') || /^[A-Za-z]:[\\/]/.test(value);
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
		const name = deviceName.trim();
		if (name.length === 0) {
			errorMessage = '请输入设备名，它只是给你自己看的标签。';
			return;
		}
		const dir = workdir.trim();
		if (!isAbsolutePath(dir)) {
			errorMessage = '工作目录必须是绝对路径（例如 /Users/你/work），agent 会用它做文件与终端的沙箱根。';
			return;
		}
		const hub = hubUrl.trim();
		if (!/^wss?:\/\/.+/.test(hub)) {
			errorMessage = 'hub 地址要形如 ws://127.0.0.1:7300。';
			return;
		}
		busy = true;
		try {
			const result = await createConnector(token, name);
			createdConnectorId = result.connector.id;
			createdToken = result.connectorToken;
			liveConnector = result.connector;
			step = 'command';
			copyState = 'idle';
			void workspace.load(token);
		} catch (error) {
			errorMessage = describeError(error);
		} finally {
			busy = false;
		}
	}

	function beginWait(): void {
		step = 'waiting';
		startPoll(createdConnectorId);
	}

	function restart(): void {
		stopPoll();
		step = 'name';
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

<section class="connect">
	<header>
		<a class="back" href="#/" title="返回任务台">←</a>
		<div>
			<h1>连接设备</h1>
			<p>
				设备主动出站连 hub，所以「连上」这件事只能在 hub 侧看到。下面三步走完，这台设备才会出现在侧栏与新建会话里。
			</p>
		</div>
	</header>

	<ol class="steps" aria-label="连接设备的三个步骤">
		<li class:active={step === 'name'} class:done={step !== 'name'}>1 · 建设备</li>
		<li class:active={step === 'command'} class:done={step === 'waiting' || step === 'connected' || step === 'timeout'}>
			2 · 拿命令
		</li>
		<li class:active={step === 'waiting' || step === 'timeout'} class:done={step === 'connected'}>
			3 · 等上线
		</li>
	</ol>

	{#if errorMessage}
		<p class="alert" role="alert">{errorMessage}</p>
	{/if}

	{#if step === 'name'}
		<form onsubmit={submitDevice}>
			<label class="field">
				<span>设备名</span>
				<input class="input" bind:value={deviceName} placeholder="例如 我的 MacBook" autocomplete="off" />
			</label>
			<p class="why">
				为什么先建设备：设备名只是给你自己看的标签，真正起作用的是一次性 token —— hub 只存它的 sha256，所以明文必须在这一步先拿到。
			</p>

			<label class="field">
				<span>hub 地址（agent 侧要连的地址）</span>
				<input class="input mono" bind:value={hubUrl} spellcheck="false" autocomplete="off" />
			</label>

			<label class="field">
				<span>工作目录（绝对路径）</span>
				<input class="input mono" bind:value={workdir} placeholder="/Users/you/work" spellcheck="false" autocomplete="off" />
			</label>
			<p class="why">
				为什么要填这两项：agent 主动去连 hub，所以地址得是设备自己能到达的 <code class="mono">ws(s)://</code>，不一定等于浏览器里的地址；工作目录同时是会话默认 cwd、文件浏览的沙箱根和终端起始目录，非绝对路径 agent 会拒绝启动。
			</p>

			<label class="field">
				<span>运行时（命令里用哪个）</span>
				<select
					class="input"
					value={runtime}
					onchange={(event) => {
						const target = event.currentTarget;
						if (target instanceof HTMLSelectElement && target.value === 'codex') runtime = 'codex';
						else runtime = 'dsh';
					}}
				>
					<option value="dsh">DeepSeek Harness · dsh</option>
					<option value="codex">Codex · codex</option>
				</select>
			</label>
			<p class="why">
				为什么要选：<code class="mono">--runtime</code> 决定 agent 拉起哪种后端；选错的话设备上线了也报不出你要的那个运行时。
			</p>

			<button class="btn" data-variant="primary" type="submit" disabled={busy}>
				{busy ? '正在创建设备…' : '创建设备并生成命令'}
			</button>
		</form>
	{:else if step === 'command'}
		<div class="card">
			<p class="card-title">1. 在这台设备上执行下面这条命令</p>
			<textarea class="input mono command" readonly rows="4" bind:this={commandField} value={command}></textarea>
			<div class="row">
				<button class="btn" type="button" onclick={() => void copyCommand()}>复制命令</button>
				<button class="btn" data-variant="primary" type="button" onclick={beginWait}>
					我已在设备上执行，开始检测
				</button>
			</div>
			{#if copyState === 'done'}
				<p class="hint ok-line">命令已复制到剪贴板。</p>
			{:else if copyState === 'manual'}
				<p class="hint warn-line">
					浏览器没有允许写入剪贴板，命令已选中：请按系统复制快捷键（⌘/Ctrl + C）或右键复制。
				</p>
			{/if}
			<p class="why">
				为什么要先复制：token 明文只出现在刚才那次响应里（hub 只存 sha256），离开本页就再也取不回来；丢了只能重新创建设备换新 token。
			</p>
			<p class="hint">
				token 只显示这一次 · 命令里的 <code class="mono">--workdir</code> 就是上一步填的
				<code class="mono">{workdir.trim()}</code>，换目录请回去重填。
			</p>
		</div>
	{:else if step === 'waiting'}
		<div class="card">
			<p class="card-title">2. 正在等设备上线</p>
			<p>
				每 2 秒查一次 <code class="mono">GET /connectors</code>，最多等 90 秒，已经等了
				<strong data-testid="connect-elapsed">{elapsedSeconds}</strong> 秒。
			</p>
			<p class="why">
				为什么要等：agent 主动出站连 hub，浏览器这边看不到它；只有 hub 记录的
				<code class="mono">status</code> 变成 <code class="mono">online</code> 才算真的连上。
			</p>
			{#if pollError}
				<p class="alert" role="alert">查询设备状态失败：{pollError}</p>
			{/if}
			<div class="row">
				<button class="btn" type="button" onclick={restart}>放弃，重新开始</button>
			</div>
		</div>
	{:else if step === 'connected'}
		<div class="card ok-card">
			<p class="card-title ok-line">3. 设备已连接</p>
			{#if liveConnector}
				<p>
					<strong>{liveConnector.name}</strong>
					<span class="badge" data-tone="ok">online</span>
					<span class="mono id">{liveConnector.id}</span>
				</p>
				<RuntimeAvailability connector={liveConnector} mode="full" />
			{/if}
			<p class="why">
				为什么这里要列运行时：会话只能用设备真正探到的运行时启动；没列出来的，建会话时也点不到。
			</p>
			<div class="row">
				<a class="btn" data-variant="primary" href="#/new-session">去新建会话</a>
				<button class="btn" data-variant="ghost" type="button" onclick={restart}>再连一台设备</button>
			</div>
		</div>
	{:else}
		<div class="card warn-card">
			<p class="card-title warn-line">3. 90 秒内没等到设备上线</p>
			<p>
				设备 <span class="mono">{createdConnectorId}</span>
				仍然是 <strong>{liveConnector?.status ?? 'offline'}</strong>，hub 没收到它的连入。
			</p>
			{#if pollError}
				<p class="alert" role="alert">最后一次查询失败：{pollError}</p>
			{/if}
			<ol class="checklist">
				<li>
					命令是否真的在跑：回设备上看那个终端，<code class="mono">cargo run -p farwork-agent</code>
					是否已经退出。缺 token、<code class="mono">--workdir</code> 不是绝对路径都会让 agent 以退出码 1 结束。
				</li>
				<li>
					hub 地址是否可达：在设备上确认 <code class="mono">{hubUrl.trim()}</code> 能连上（hub 默认监听 7300）。
					地址里的 host 必须是设备自己网络里能解析到的那台机器，不是浏览器里的 localhost。
				</li>
				<li>
					日志在哪：agent 的日志就是它自己那个终端的 stdout/stderr，它不写独立日志文件。
					<code class="mono">$FARWORK_HOME/agent.json</code>（默认 <code class="mono">~/.farwork/agent.json</code>）只是身份文件，不含 token，删掉会换一个新的 agent_id。
				</li>
				<li>
					token 是否用错：token 只显示过一次；如果命令里的 token 抄错或被别的设备先用过，hub 会拒绝这条连接。
					需要新 token 就回上一步重新创建设备。
				</li>
			</ol>
			<div class="row">
				<button class="btn" data-variant="primary" type="button" onclick={() => beginWait()}>再等 90 秒</button>
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

	select.input {
		height: 38px;
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
		min-height: 76px;
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

	.why {
		font-size: 11.5px;
		color: var(--muted-foreground);
		line-height: 1.65;
		padding-left: 10px;
		border-left: 2px solid var(--border);
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
