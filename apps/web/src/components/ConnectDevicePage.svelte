<script lang="ts">
	import { API_NAMESPACE } from '$lib/api/client';
	import { fetchHubMeta } from '$lib/api/meta';
	import { fetchPairing, startPairing, type PairingCode } from '$lib/api/pairing';
	import { listConnectors } from '$lib/api/workspace';
	import { workspace } from '$lib/workspace.svelte';
	import { describeError } from '$lib/format';
	import RuntimeAvailability from './RuntimeAvailability.svelte';
	import type { Connector } from '$lib/api/types';

	let { token }: { token: string } = $props();

	type Step = 'generate' | 'waiting' | 'claimed' | 'connected' | 'expired' | 'lost';
	type Runtime = 'codex' | 'dsh' | 'echo';

	const AGENT_DEFAULT_RUNTIME: Runtime = 'codex';
	const PAIRING_POLL_MS = 2_000;
	const DEVICE_POLL_MS = 2_000;
	const DEVICE_WAIT_MS = 90_000;
	/** 仓库路径在浏览器里无从得知：只在开发者折叠块里留占位符。 */
	const REPO_PLACEHOLDER = '/path/to/farwork';
	const API_SUFFIX = '/api/v2';
	const RUNTIMES: { value: Runtime; label: string }[] = [
		{ value: 'codex', label: 'Codex' },
		{ value: 'dsh', label: 'dsh' },
		{ value: 'echo', label: 'echo' }
	];

	// 回退用的 origin：优先 PUBLIC_FARWORK_API（构建期指定了别的 host 时以它为准），
	// 否则就是当前页面 origin。它只在 hub 自报地址拿不到时才进入命令 —— dev 下页面在
	// 5173、hub 在 7300（vite 代理），反代下更是两个域名，把页面 origin 当 hub 写进
	// `--hub` 会让 agent 把它存进 agent.json 然后永远连不上。
	function apiOrigin(): string {
		const base = API_NAMESPACE.endsWith(API_SUFFIX)
			? API_NAMESPACE.slice(0, -API_SUFFIX.length)
			: '';
		if (base.length > 0) return base;
		return typeof window === 'undefined' ? '' : window.location.origin;
	}

	/** 本页 origin：只在拿不到 hub 自报地址时兜底，用上了要在界面上标注。 */
	const installOrigin = apiOrigin();

	let step = $state<Step>('generate');
	let hubUrl = $state('');
	/** hub 自报的基地址（GET /api/v2/meta）；null = 还没拿到或拿不到。 */
	let hubBaseFromMeta = $state<string | null>(null);
	let workdir = $state('');
	let runtime = $state<Runtime>(AGENT_DEFAULT_RUNTIME);
	let busy = $state(false);
	let errorMessage = $state<string | null>(null);
	let pollError = $state<string | null>(null);
	let pairing = $state<PairingCode | null>(null);
	let claimedConnectorId = $state<string | null>(null);
	let liveConnector = $state<Connector | null>(null);
	let clockMs = $state(Date.now());
	let serverOffsetMs = $state(0);
	let expiredNote = $state('');
	let copyState = $state<'idle' | 'done' | 'manual'>('idle');
	let commandField = $state<HTMLTextAreaElement | null>(null);

	// 轮询只活在这个向导页里，离开即停；generation 用来丢弃旧一轮的迟到响应。
	let generation = 0;
	let countdownTimer: ReturnType<typeof setInterval> | null = null;
	let pairingTimer: ReturnType<typeof setInterval> | null = null;
	let deviceTimer: ReturnType<typeof setInterval> | null = null;
	/** 生成配对码之前的设备集合：claimed 但 hub 没给 connectorId 时用它认出「新上线的那台」。 */
	let knownConnectorIds: string[] = [];

	/** 命令里的 hub 地址：手填 > hub 自报 > 本页 origin（回退，界面要标注）。 */
	const hubBase = $derived(hubUrl.trim() || hubBaseFromMeta || installOrigin);
	const usingFallbackHub = $derived(hubUrl.trim().length === 0 && hubBaseFromMeta === null);
	const installCommand = $derived(`curl -fsSL ${hubBase}/install.sh | sh`);
	const pairCommand = $derived.by(() => {
		if (pairing === null) return '';
		const parts = ['farwork-agent', 'pair', '--hub', hubBase, '--code', pairing.code];
		const dir = workdir.trim();
		if (dir.length > 0) parts.push('--workdir', dir);
		if (runtime !== AGENT_DEFAULT_RUNTIME) parts.push('--runtime', runtime);
		return parts.join(' ');
	});
	const commandText = $derived(pairing === null ? '' : `${installCommand}\n${pairCommand}`);
	const sourceCommand = $derived(
		pairing === null ? '' : `cd ${REPO_PLACEHOLDER} && ${pairCommand.replace(/^farwork-agent/, 'cargo run -p farwork-agent --')}`
	);
	const remainingMs = $derived.by(() => {
		if (pairing === null) return 0;
		const expiresAt = Date.parse(pairing.expiresAt);
		if (Number.isNaN(expiresAt)) return 0;
		return Math.max(0, expiresAt - (clockMs + serverOffsetMs));
	});
	const remainingLabel = $derived(formatRemaining(remainingMs));

	function formatRemaining(ms: number): string {
		const total = Math.max(0, Math.ceil(ms / 1000));
		const minutes = Math.floor(total / 60);
		const seconds = total % 60;
		return `${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
	}

	// fetch 失败只给一句 "Failed to fetch"：补上它连的是谁，用户才知道要看哪里。
	function pairingError(error: unknown): string {
		if (error instanceof TypeError) {
			return `连不上 ${installOrigin}：${error.message}（确认 hub 在跑，地址与协议对得上）`;
		}
		return describeError(error);
	}

	// 拿不到就回退到本页 origin；失败本身不报错，界面上的「这是回退值」标注更有用。
	async function loadMeta(): Promise<void> {
		try {
			const meta = await fetchHubMeta();
			hubBaseFromMeta = meta?.hubUrl ?? null;
		} catch {
			hubBaseFromMeta = null;
		}
	}

	function isAbsolutePath(value: string): boolean {
		return value.startsWith('/') || /^[A-Za-z]:[\\/]/.test(value);
	}

	function stopTimers(): void {
		generation += 1;
		if (countdownTimer !== null) clearInterval(countdownTimer);
		if (pairingTimer !== null) clearInterval(pairingTimer);
		if (deviceTimer !== null) clearInterval(deviceTimer);
		countdownTimer = null;
		pairingTimer = null;
		deviceTimer = null;
	}

	$effect(() => () => stopTimers());
	// 进页面就取一次：还没生成配对码时，高级选项的占位符也该是 hub 的真实地址。
	$effect(() => {
		void loadMeta();
	});


	function startCountdown(): void {
		const current = generation;
		clockMs = Date.now();
		countdownTimer = setInterval(() => {
			if (current !== generation) return;
			clockMs = Date.now();
			// 本地先到时也立刻收尾：不能让界面在一个已过期的码上继续假装在等。
			if (remainingMs <= 0 && (step === 'waiting' || step === 'claimed')) {
				stopTimers();
				expiredNote = '倒计时已走完，服务端也不会再接受这个码。';
				step = 'expired';
			}
		}, 1_000);
	}

	function applySnapshot(
		status: 'pending' | 'claimed' | 'expired' | 'unknown',
		connectorId: string | null
	): void {
		if (status === 'pending') {
			step = 'waiting';
			return;
		}
		if (status === 'claimed') {
			claimedConnectorId = connectorId;
			step = 'claimed';
			if (pairingTimer !== null) clearInterval(pairingTimer);
			pairingTimer = null;
			startDevicePoll();
			return;
		}
		stopTimers();
		expiredNote =
			status === 'expired'
				? '服务端已经把这个码标成过期。'
				: 'hub 不认识这个配对码：它可能属于另一个 hub 实例，或者数据被重建过。';
		step = 'expired';
	}

	function startPairingPoll(): void {
		const current = generation;
		const tick = async (): Promise<void> => {
			if (current !== generation || pairing === null) return;
			try {
				const result = await fetchPairing(token, pairing.id);
				if (current !== generation) return;
				pollError = null;
				serverOffsetMs = result.serverNowMs - Date.now();
				applySnapshot(result.pairing.status, result.pairing.connectorId);
			} catch (error) {
				if (current !== generation) return;
				pollError = pairingError(error);
			}
		};
		void tick();
		pairingTimer = setInterval(() => void tick(), PAIRING_POLL_MS);
	}

	function resolveClaimedConnector(rows: Connector[]): Connector | null {
		if (claimedConnectorId !== null) {
			return rows.find((row) => row.id === claimedConnectorId) ?? null;
		}
		return rows.find((row) => row.status === 'online' && !knownConnectorIds.includes(row.id)) ?? null;
	}

	function startDevicePoll(): void {
		const current = generation;
		const deadline = Date.now() + DEVICE_WAIT_MS;
		const tick = async (): Promise<void> => {
			if (current !== generation) return;
			try {
				const rows = await listConnectors(token);
				if (current !== generation) return;
				pollError = null;
				const row = resolveClaimedConnector(rows);
				if (row !== null) liveConnector = row;
				if (row !== null && row.status === 'online') {
					stopTimers();
					step = 'connected';
					void workspace.load(token);
					return;
				}
			} catch (error) {
				if (current !== generation) return;
				pollError = pairingError(error);
			}
			if (current !== generation) return;
			if (Date.now() >= deadline) {
				stopTimers();
				step = 'lost';
			}
		};
		void tick();
		deviceTimer = setInterval(() => void tick(), DEVICE_POLL_MS);
	}

	async function beginPairing(): Promise<void> {
		errorMessage = null;
		pollError = null;
		const hub = hubUrl.trim();
		if (hub.length > 0 && !/^https?:\/\/.+/i.test(hub)) {
			errorMessage = `hub 地址要形如 ${installOrigin}：pair 用 http(s)，ws 地址由它自己算。`;
			return;
		}
		const dir = workdir.trim();
		if (dir.length > 0 && !isAbsolutePath(dir)) {
			errorMessage = '工作目录要填绝对路径（例如 /Users/you/work），或者留空用 agent 当前目录。';
			return;
		}
		busy = true;
		stopTimers();
		try {
			// 每次生成都重新问一次：hub 可能刚换过地址，别把上一条命令里的旧地址接着用。
			if (hub.length === 0) await loadMeta();
			knownConnectorIds = (await listConnectors(token)).map((row) => row.id);
			const started = await startPairing(token);
			pairing = started.pairing;
			serverOffsetMs = started.serverNowMs - Date.now();
			claimedConnectorId = null;
			liveConnector = null;
			expiredNote = '';
			copyState = 'idle';
			pollError = null;
			step = 'waiting';
			startCountdown();
			startPairingPoll();
		} catch (error) {
			errorMessage = pairingError(error);
			step = 'generate';
		} finally {
			busy = false;
		}
	}

	function retryWait(): void {
		pollError = null;
		step = 'claimed';
		startDevicePoll();
	}

	function restart(): void {
		stopTimers();
		step = 'generate';
		pairing = null;
		claimedConnectorId = null;
		liveConnector = null;
		pollError = null;
		expiredNote = '';
		copyState = 'idle';
		errorMessage = null;
	}

	function selectCommand(): void {
		commandField?.focus();
		commandField?.select();
	}

	async function copyCommand(): Promise<void> {
		try {
			await navigator.clipboard.writeText(commandText);
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
			<p>生成一个 8 位配对码，在设备上执行两行命令即可，不用装 Rust、不用源码、不用抄 token。</p>
		</div>
	</header>

	{#if step !== 'generate'}
		<ol class="steps" aria-label="连接设备的步骤">
			<li class:done={pairing !== null}>1 · 生成配对码</li>
			<li
				class:active={step === 'waiting' || step === 'claimed'}
				class:done={step === 'connected'}
			>
				2 · 设备配对
			</li>
			<li class:active={step === 'claimed'} class:done={step === 'connected'}>3 · 设备上线</li>
		</ol>
	{/if}

	{#if errorMessage}
		<p class="alert" role="alert">{errorMessage}</p>
	{/if}

	{#if step === 'generate'}
		<form onsubmit={(event) => {
			event.preventDefault();
			void beginPairing();
		}}>
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
					dsh 连本机 DSH 桥（要先在 DSH 里安装并启用 @farwork/dsh-bridge 插件）；echo 仅联调，不执行任何东西。
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
							placeholder={hubBaseFromMeta ?? installOrigin}
						/>
					</label>
					<p class="hint">
						只影响命令里的 <code class="mono">--hub</code>；留空就用 hub 自报的地址（
						<code class="mono">GET /api/v2/meta</code>）。
					</p>

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
				{busy ? '正在生成配对码…' : '生成配对码'}
			</button>
		</form>
	{:else if step === 'waiting' || step === 'claimed'}
		<div class="card">
			<p class="card-title">把这台设备的配对码输给它</p>
			<p class="code mono" data-testid="connect-code">{pairing?.code ?? ''}</p>
			<p class="hint">
				配对码 <strong data-testid="connect-countdown">{remainingLabel}</strong> 后过期（只显示这一次，离开本页就取不回来）。
			</p>

			<p class="card-title">也可以直接在这台设备上执行这两行</p>
			<textarea
				class="input mono command"
				data-testid="connect-command"
				readonly
				rows="2"
				bind:this={commandField}
				value={commandText}
			></textarea>
			{#if usingFallbackHub}
				<p class="hint warn-line" data-testid="connect-fallback">
					拿不到 hub 自报的地址（<code class="mono">GET /api/v2/meta</code> 失败），上面的命令用的是本页面
					origin <code class="mono">{installOrigin}</code>。本页不是 hub 时（dev 在 5173、hub 在 7300，
					反代下更是两个域名）命令里的 <code class="mono">--hub</code> 就是错的：在高级选项里手填 hub
					地址再生成一次。
				</p>
			{/if}
			<div class="row">
				<button class="btn" data-variant="primary" type="button" onclick={() => void copyCommand()}>
					复制命令
				</button>
				<button class="btn" data-variant="ghost" type="button" onclick={() => void beginPairing()}>
					重新生成
				</button>
			</div>
			{#if copyState === 'done'}
				<p class="hint ok-line">命令已复制到剪贴板。</p>
			{:else if copyState === 'manual'}
				<p class="hint warn-line">
					浏览器没有允许写入剪贴板，命令已选中：请按系统复制快捷键（⌘/Ctrl + C）或右键复制。
				</p>
			{/if}

			<details class="advanced" data-testid="connect-source">
				<summary>从源码运行（开发者）</summary>
				<div class="advanced-body">
					<pre class="mono source-command" data-testid="connect-source-command">{sourceCommand}</pre>
					<p class="hint">
						把 <code class="mono">{REPO_PLACEHOLDER}</code> 换成本仓库路径；这条路依赖本机 Rust 工具链，客户不用。
					</p>
				</div>
			</details>

			{#if step === 'claimed'}
				<p class="hint ok-line" data-testid="connect-claimed">该码已被领取，正在等设备上线…</p>
			{:else}
				<p class="hint">执行完不用点按钮：这里每 2 秒查一次配对状态。</p>
			{/if}
			{#if pollError}
				<p class="alert" role="alert">查询配对状态失败：{pollError}</p>
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
	{:else if step === 'expired'}
		<div class="card warn-card">
			<p class="card-title warn-line" data-testid="connect-expired">配对码已过期</p>
			<p>{expiredNote}</p>
			<p class="hint">过期后这个码不会再被任何设备领取，换一个新码即可。</p>
			<div class="row">
				<button class="btn" data-variant="primary" type="button" onclick={() => void beginPairing()}>
					重新生成
				</button>
				<button class="btn" data-variant="ghost" type="button" onclick={restart}>返回</button>
			</div>
		</div>
	{:else}
		<div class="card warn-card">
			<p class="card-title warn-line" data-testid="connect-lost">该码已被领取，但设备没连上来</p>
			<p>
				码已经绑定到 {claimedConnectorId ?? '某台设备'}，它 {DEVICE_WAIT_MS / 1000} 秒内没有连上 hub；
				如果领走它的不是你正在配的这台机器，就得换一个新码。
			</p>
			{#if pollError}
				<p class="alert" role="alert">最后一次查询失败：{pollError}</p>
			{/if}
			<ol class="checklist">
				<li>
					命令还在跑吗：回设备那个终端看 agent 是否已退出（缺 token、<code class="mono">--workdir</code>
					不是绝对路径都会以退出码 1 结束）。
				</li>
				<li>
					hub 地址通吗：<code class="mono">{hubBase}</code> 要能从设备上连到。
				</li>
				<li>
					配对码有 TTL，领了就作废一次：客户端要重试得重新生成一个码。
				</li>
			</ol>
			<div class="row">
				<button class="btn" data-variant="primary" type="button" onclick={retryWait}>
					再等 {DEVICE_WAIT_MS / 1000} 秒
				</button>
				<button class="btn" data-variant="ghost" type="button" onclick={() => void beginPairing()}>
					重新生成
				</button>
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

	.code {
		font-size: 34px;
		font-weight: 600;
		letter-spacing: 0.18em;
		line-height: 1.2;
		color: var(--foreground);
		user-select: all;
	}

	.command {
		height: auto;
		min-height: 56px;
		padding: 10px 12px;
		line-height: 1.6;
		resize: vertical;
		font-size: 11.5px;
		word-break: break-all;
	}

	.source-command {
		margin: 0;
		padding: 10px 12px;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border);
		background: oklch(1 0 0 / 4%);
		font-size: 11px;
		line-height: 1.6;
		white-space: pre-wrap;
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