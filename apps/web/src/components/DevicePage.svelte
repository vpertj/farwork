<script lang="ts">
	import { ApiError } from '$lib/api/client';
	import {
		countConnectorChildren,
		deleteConnector,
		revokeConnectorToken,
		type ConnectorChildren
	} from '$lib/api/workspace';
	import { describeError, formatRelative } from '$lib/format';
	import { workspace } from '$lib/workspace.svelte';
	import RuntimeAvailability from './RuntimeAvailability.svelte';

	let { connectorId, token }: { connectorId: string; token: string } = $props();

	const connector = $derived(workspace.connectorById(connectorId));
	const online = $derived(connector?.status === 'online');
	const deviceSessions = $derived(
		workspace.sessions.filter((session) => session.connectorId === connectorId)
	);
	const deviceProjects = $derived(
		workspace.projects.filter((project) => project.connectorId === connectorId)
	);

	let dialogOpen = $state(false);
	let counting = $state(false);
	let counts = $state<ConnectorChildren | null>(null);
	let countError = $state<string | null>(null);
	let acknowledged = $state(false);
	let deleting = $state(false);
	let deleteError = $state<string | null>(null);
	let deleted = $state<{ name: string; sessions: number; projects: number } | null>(null);

	let revokeOpen = $state(false);
	let revoking = $state(false);
	let revokeError = $state<string | null>(null);
	let revokedToken = $state<string | null>(null);
	let revokeCopy = $state<'idle' | 'done' | 'manual'>('idle');
	let revokeField = $state<HTMLTextAreaElement | null>(null);

	let notice = $state<string | null>(null);

	// 删设备是破坏性的（hub 会连带删掉会话与项目且没有回收站），所以错误必须原样
	// 展示后端的 status/code：403/404 与「连不上」是三种完全不同的处置方式。
	// 路由从一台设备换到另一台时组件实例会复用：不清掉就会把上一台的 token
	// 显示在下一台设备页上（凭证串页），也会带着上一台弹窗的状态。
	let activeDevice = '';
	$effect(() => {
		const current = connectorId;
		if (activeDevice === current) return;
		activeDevice = current;
		dialogOpen = false;
		counting = false;
		counts = null;
		countError = null;
		acknowledged = false;
		deleting = false;
		deleteError = null;
		deleted = null;
		revokeOpen = false;
		revoking = false;
		revokeError = null;
		revokedToken = null;
		revokeCopy = 'idle';
		notice = null;
	});

	function describeDeviceError(error: unknown): string {
		if (error instanceof ApiError) {
			const code = error.code ? ` · ${error.code}` : '';
			return `HTTP ${error.status}${code}：${error.message}`;
		}
		return describeError(error);
	}

	async function openDeleteDialog(): Promise<void> {
		notice = null;
		deleteError = null;
		acknowledged = false;
		counts = null;
		countError = null;
		dialogOpen = true;
		counting = true;
		try {
			counts = await countConnectorChildren(token, connectorId);
		} catch (error) {
			countError = describeDeviceError(error);
		} finally {
			counting = false;
		}
	}

	function closeDeleteDialog(): void {
		if (deleting) return;
		dialogOpen = false;
		acknowledged = false;
		counts = null;
		countError = null;
		deleteError = null;
	}

	async function confirmDelete(): Promise<void> {
		if (!connector || counts === null || !acknowledged) return;
		deleting = true;
		deleteError = null;
		const removed = { name: connector.name, sessions: counts.sessions, projects: counts.projects };
		try {
			await deleteConnector(token, connectorId);
			dialogOpen = false;
			deleted = removed;
			notice = `设备「${removed.name}」已删除。`;
			void workspace.load(token);
		} catch (error) {
			deleteError = describeDeviceError(error);
		} finally {
			deleting = false;
		}
	}

	function openRevoke(): void {
		notice = null;
		revokeError = null;
		revokeCopy = 'idle';
		revokeOpen = true;
	}

	function closeRevoke(): void {
		if (revoking) return;
		revokeOpen = false;
		revokeError = null;
	}

	async function confirmRevoke(): Promise<void> {
		if (!connector) return;
		revoking = true;
		revokeError = null;
		try {
			const result = await revokeConnectorToken(token, connectorId);
			revokedToken = result.connectorToken;
			revokeOpen = false;
			notice = `已为「${result.connector.name}」重新签发 token：旧 token 立刻失效，必须在设备上换新的。`;
			void workspace.load(token);
		} catch (error) {
			revokeError = describeDeviceError(error);
		} finally {
			revoking = false;
		}
	}

	async function copyToken(): Promise<void> {
		if (!revokedToken) return;
		try {
			await navigator.clipboard.writeText(revokedToken);
			revokeCopy = 'done';
		} catch {
			// 非安全上下文/权限被拒：退化为「已选中，手动复制」，同 ShareLinkCard。
			revokeCopy = 'manual';
			revokeField?.focus();
			revokeField?.select();
		}
	}

	function forgetToken(): void {
		revokedToken = null;
		revokeCopy = 'idle';
	}
</script>

<section class="device-page">
	<header>
		<a class="back" href="#/projects" title="返回项目">←</a>
		<div>
			<h1>{deleted ? deleted.name : (connector?.name ?? '设备')}</h1>
			<p class="sub mono">{connectorId}</p>
		</div>
	</header>

	{#if notice}
		<p class="alert notice" role="status">{notice}</p>
	{/if}

	{#if deleted}
		<div class="card ok-card">
			<p class="card-title">设备已删除</p>
			<p>
				连同它下面的 {deleted.sessions} 个会话和 {deleted.projects} 个项目一起删除了（hub 的
				<code class="mono">DELETE /connectors/{connectorId}</code> 没有回收站）。
			</p>
			<p class="hint">左侧列表已经不再有这台设备。设备侧的 agent 用旧 token 也连不上了。</p>
			<a class="btn" data-variant="primary" href="#/projects">回到项目</a>
		</div>
	{:else if !connector}
		{#if workspace.loadedAt > 0}
			<div class="card warn-card" role="alert">
				<p class="card-title">没有这台设备</p>
				<p>
					在当前的设备列表里找不到 <code class="mono">{connectorId}</code>：它可能已被删除，或者属于别的账号。
				</p>
				<a class="btn" data-variant="ghost" href="#/projects">回到项目</a>
			</div>
		{:else}
			<p class="hint">正在加载设备…</p>
		{/if}
	{:else}
		<div class="card">
			<div class="row">
				<span class="badge" data-tone={online ? 'ok' : 'muted'}>
					<span class="dot"></span>
					{online ? '在线' : '离线'}
				</span>
				<span class="hint">
					{connector.lastSeenAt ? `最后在线 ${formatRelative(connector.lastSeenAt)}` : '从未连上 hub'}
				</span>
				{#if connector.deviceOs}
					<span class="hint mono">{connector.deviceOs}</span>
				{/if}
			</div>
			<dl class="meta">
				<div>
					<dt>会话</dt>
					<dd>{deviceSessions.length}</dd>
				</div>
				<div>
					<dt>项目</dt>
					<dd>{deviceProjects.length}</dd>
				</div>
				<div>
					<dt>类型</dt>
					<dd>{connector.connectorKind}</dd>
				</div>
			</dl>
			<RuntimeAvailability {connector} mode="full" />
		</div>

		{#if revokedToken}
			<div class="card warn-card" data-testid="revoked-token">
				<p class="card-title warn-line">新的 connector token（只显示这一次）</p>
				<textarea
					class="input mono token"
					readonly
					rows="2"
					bind:this={revokeField}
					value={revokedToken}
				></textarea>
				<div class="row">
					<button class="btn" data-variant="primary" type="button" onclick={() => void copyToken()}>
						{revokeCopy === 'done' ? '已复制' : '复制 token'}
					</button>
					<button class="btn" data-variant="ghost" type="button" onclick={forgetToken}>
						我已保存，清除显示
					</button>
				</div>
				{#if revokeCopy === 'manual'}
					<p class="hint warn-line">剪贴板不可用，token 已选中：请用系统复制快捷键复制。</p>
				{/if}
				<p class="hint">
					hub 只存 sha256，关掉这条提示后这个值就取不回来了；拿不到就再签发一次。
				</p>
			</div>
		{/if}

		<div class="card">
			<p class="card-title">重新签发 token</p>
			<p class="hint">
				只换凭证、不删设备：旧 token 立刻失效，设备下次握手会被拒，要在设备上换成新的再连。
				它<strong>不会</strong>动会话与项目。
			</p>
			{#if revokeError}
				<p class="alert" role="alert">{revokeError}</p>
			{/if}
			{#if revokeOpen}
				<div class="confirm" role="alertdialog" aria-label="确认重新签发 token">
					<p class="confirm-title">给「{connector.name}」签发新的 connector token？</p>
					<p class="confirm-body">新 token 只显示一次；旧 token 立刻作废，正在连的设备会掉线。</p>
					<div class="confirm-actions">
						<button
							class="btn"
							data-variant="primary"
							type="button"
							disabled={revoking}
							onclick={() => void confirmRevoke()}
						>
							{revoking ? '签发中…' : '确认重新签发'}
						</button>
						<button class="btn" data-variant="ghost" type="button" disabled={revoking} onclick={closeRevoke}>
							取消
						</button>
					</div>
				</div>
			{:else}
				<div class="row">
					<button class="btn" type="button" onclick={openRevoke}>重新签发 token</button>
				</div>
			{/if}
		</div>

		<div class="card danger-card">
			<p class="card-title err-line">删除设备</p>
			<p class="hint">
				这是破坏性操作：hub 先删这台设备的会话、再删项目、最后删设备本身，<strong>没有回收站</strong>。
				只是想换凭证的话，用上面的「重新签发 token」。
			</p>
			<div class="row">
				<button
					class="btn danger"
					type="button"
					data-testid="open-delete-device"
					onclick={() => void openDeleteDialog()}
				>
					删除设备…
				</button>
			</div>
		</div>
	{/if}
</section>

{#if dialogOpen && connector}
	<div class="overlay" role="presentation">
		<div class="dialog" role="dialog" aria-modal="true" aria-label="确认删除设备">
			<p class="dialog-title">删除设备「{connector.name}」</p>

			{#if counting}
				<p class="hint" data-testid="delete-counting">正在读取这台设备下的真实会话与项目数量…</p>
			{:else if countError}
				<p class="alert" role="alert">取不到真实计数，因此不允许删除：{countError}</p>
				<div class="row">
					<button class="btn" type="button" onclick={() => void openDeleteDialog()}>重试</button>
					<button class="btn" data-variant="ghost" type="button" onclick={closeDeleteDialog}>
						取消
					</button>
				</div>
			{:else if counts}
				<p class="dialog-body" data-testid="delete-summary">
					将删除设备「{connector.name}」，以及它下面的 {counts.sessions} 个会话和 {counts.projects}
					个项目。此操作不可恢复。
				</p>
				{#if counts.sessionsCapped}
					<p class="hint warn-line">
						会话清单一次最多返回 100 条，上面的会话数只是下界：实际会删除的会话可能更多。
					</p>
				{/if}
				<label class="ack">
					<input
						type="checkbox"
						bind:checked={acknowledged}
						data-testid="delete-acknowledge"
						disabled={deleting}
					/>
					<span>我知道会删除该设备下的全部会话与项目，且不可恢复</span>
				</label>
				{#if deleteError}
					<p class="alert" role="alert">{deleteError}</p>
				{/if}
				<div class="row">
					<button
						class="btn danger"
						type="button"
						data-testid="confirm-delete-device"
						disabled={!acknowledged || deleting}
						onclick={() => void confirmDelete()}
					>
						{deleting ? '删除中…' : '确认删除设备'}
					</button>
					<button class="btn" data-variant="ghost" type="button" disabled={deleting} onclick={closeDeleteDialog}>
						取消
					</button>
				</div>
			{/if}
		</div>
	</div>
{/if}

<svelte:window
	onkeydown={(event) => {
		if (event.key !== 'Escape') return;
		if (dialogOpen) closeDeleteDialog();
		else if (revokeOpen) closeRevoke();
	}}
/>

<style>
	.device-page {
		display: flex;
		flex-direction: column;
		gap: 14px;
		padding: 28px 32px 48px;
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

	.sub {
		margin-top: 4px;
		font-size: 11px;
		color: var(--muted-foreground);
	}

	.card {
		display: flex;
		flex-direction: column;
		gap: 10px;
		padding: 14px 16px;
		border: 1px solid var(--border);
		border-radius: var(--radius-md);
		background: var(--card);
		font-size: 12.5px;
		line-height: 1.65;
	}

	.card-title {
		font-size: 13px;
		font-weight: 600;
	}

	.ok-card {
		border-color: color-mix(in oklch, var(--ok) 42%, transparent);
		background: color-mix(in oklch, var(--ok) 10%, transparent);
	}

	.warn-card {
		border-color: color-mix(in oklch, var(--warn) 42%, transparent);
		background: color-mix(in oklch, var(--warn) 8%, transparent);
	}

	.danger-card {
		border-color: color-mix(in oklch, var(--err) 45%, transparent);
		background: color-mix(in oklch, var(--err) 8%, transparent);
	}

	.notice {
		border-color: color-mix(in oklch, var(--ok) 45%, transparent);
		background: color-mix(in oklch, var(--ok) 12%, transparent);
	}

	.row {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 8px;
	}

	.meta {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
		gap: 6px 18px;
		margin: 0;
		font-size: 12px;
	}

	.meta dt {
		font-size: 10.5px;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--muted-foreground);
	}

	.meta dd {
		margin: 0;
	}

	.hint {
		font-size: 11.5px;
		color: var(--muted-foreground);
	}

	.warn-line {
		color: var(--warn);
	}

	.err-line {
		color: var(--err);
	}

	.confirm {
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 12px 14px;
		border-radius: var(--radius-sm);
		border: 1px solid color-mix(in oklch, var(--err) 45%, transparent);
		background: color-mix(in oklch, var(--err) 10%, transparent);
	}

	.confirm-title {
		font-weight: 600;
	}

	.confirm-body {
		color: var(--muted-foreground);
	}

	.confirm-actions {
		display: flex;
		gap: 6px;
	}

	.btn.danger {
		border-color: color-mix(in oklch, var(--err) 55%, transparent);
		background: color-mix(in oklch, var(--err) 22%, transparent);
		color: var(--foreground);
	}

	.overlay {
		position: fixed;
		inset: 0;
		z-index: 40;
		display: grid;
		place-items: center;
		padding: 24px;
		background: oklch(0 0 0 / 62%);
	}

	.dialog {
		display: flex;
		flex-direction: column;
		gap: 10px;
		width: min(520px, 100%);
		padding: 18px 20px 20px;
		border-radius: var(--radius-lg);
		border: 1px solid color-mix(in oklch, var(--err) 45%, transparent);
		background: var(--card);
		box-shadow: 0 24px 60px oklch(0 0 0 / 55%);
		font-size: 12.5px;
		line-height: 1.7;
	}

	.dialog-title {
		font-size: 14px;
		font-weight: 600;
	}

	.dialog-body {
		color: var(--foreground);
	}

	.ack {
		display: flex;
		align-items: flex-start;
		gap: 8px;
		padding: 8px 10px;
		border-radius: var(--radius-sm);
		border: 1px solid var(--border);
		background: oklch(1 0 0 / 3%);
		font-size: 12px;
	}

	.ack input {
		margin-top: 2px;
	}

	.token {
		height: auto;
		min-height: 52px;
		padding: 8px 10px;
		font-size: 11.5px;
		line-height: 1.6;
		word-break: break-all;
		resize: vertical;
	}
</style>