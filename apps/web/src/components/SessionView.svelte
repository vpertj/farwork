<script lang="ts">
	import { SessionController } from '$lib/session.svelte';
	import { createSessionShare } from '$lib/api/shares';
	import type { SessionShare } from '$lib/api/types';
	import { absoluteHref } from '$lib/router';
	import { describeError, formatClock } from '$lib/format';
	import {
		itemHeadline,
		runtimeLabel,
		sessionStatusLabel,
		sessionStatusTone
	} from '$lib/timeline';
	import { workspace } from '$lib/workspace.svelte';
	import ApprovalCard from './ApprovalCard.svelte';
	import FilePanel from './FilePanel.svelte';
	import TerminalPanel from './TerminalPanel.svelte';
	import TimelineEntry from './TimelineEntry.svelte';
	import ShareLinkCard from './ShareLinkCard.svelte';

	let { sessionId, token }: { sessionId: string; token: string } = $props();

	const controller = new SessionController();
	let draft = $state('');
	let scroller: HTMLDivElement | null = $state(null);
	let stickToBottom = true;

	const session = $derived(controller.session);
	const status = $derived(controller.runtimeState?.status ?? session?.status ?? 'idle');
	const canInterrupt = $derived(status === 'running' || status === 'waiting_approval');
	const showApprovals = $derived(
		controller.approvalNotices.length > 0 || controller.pendingApprovalItems.length > 0
	);
	const project = $derived(workspace.projectById(session?.projectId ?? null));
	const workspacePath = $derived(project?.workspacePath ?? session?.cwd ?? '');
	const connectorOnline = $derived((session?.connectorStatus ?? 'offline') === 'online');

	type SidePanel = 'none' | 'files' | 'terminal';
	let panel = $state<SidePanel>('files');

	function togglePanel(target: SidePanel): void {
		panel = panel === target ? 'none' : target;
	}

	const connectionLabel = $derived(
		controller.connection === 'open'
			? '实时连接'
			: controller.connection === 'connecting'
				? '连接中…'
				: controller.connection === 'reconnecting'
					? '重连中…'
					: controller.connection === 'closed'
						? '已断开'
						: '未连接'
	);
	const connectionTone = $derived(
		controller.connection === 'open'
			? 'ok'
			: controller.connection === 'reconnecting' || controller.connection === 'connecting'
				? 'warn'
				: 'muted'
	);

	let activeKey = '';

	$effect(() => {
		const key = `${sessionId}::${token}`;
		if (activeKey === key) return;
		activeKey = key;
		void controller.open(sessionId, token);
	});

	$effect(() => () => {
		activeKey = '';
		controller.dispose();
	});

	$effect(() => {
		const count = controller.items.length + controller.optimistic.length;
		void count;
		if (scroller && stickToBottom) scroller.scrollTop = scroller.scrollHeight;
	});

	function handleScroll(): void {
		if (!scroller) return;
		const distance = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight;
		stickToBottom = distance < 96;
	}

	async function submit(): Promise<void> {
		const text = draft.trim();
		if (text.length === 0) return;
		draft = '';
		stickToBottom = true;
		await controller.send(text);
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
			event.preventDefault();
			void submit();
		}
	}

	let shareBusy = $state(false);
	let shareError = $state<string | null>(null);
	let shareLink = $state<SessionShare | null>(null);
	let shareLabel = $state('');

	type ShareInput = { scope: 'session' } | { scope: 'message'; messageId: string };

	async function share(input: ShareInput): Promise<void> {
		if (shareBusy) return;
		shareBusy = true;
		shareError = null;
		try {
			const created = await createSessionShare(token, sessionId, input);
			shareLink = created;
			shareLabel = created.scope === 'message' ? '单条消息分享' : '整段会话分享';
		} catch (error) {
			shareError = describeError(error);
		} finally {
			shareBusy = false;
		}
	}
</script>

<div class="session-layout">
	<section class="session">
		<header>
			<a class="back" href="#/" title="返回任务台">←</a>
			<div class="title-block">
				<h1>{session?.title ?? '会话'}</h1>
				<div class="meta mono">
					<span>{sessionId}</span>
					<span>·</span>
					<span>{runtimeLabel(session?.runtime ?? 'unknown')}</span>
					<span>·</span>
					<span>游标 seq:{controller.cursorSeq}</span>
				</div>
			</div>
			<div class="badges">
				{#if status === 'waiting_approval'}
					<span class="badge status-alert" data-tone="warn" role="status">等待审批 · 需要你决定</span>
				{:else if controller.openNotices.length > 0}
					<span class="badge" data-tone="warn">待审批 {controller.openNotices.length}</span>
				{/if}
				<span class="badge" data-tone={sessionStatusTone(status)}>
					<span class="dot"></span>
					{sessionStatusLabel(status)}
				</span>
				<span class="badge" data-tone={connectionTone}>{connectionLabel}</span>
				{#if canInterrupt}
					<button
						class="btn"
						type="button"
						disabled={controller.interrupting}
						onclick={() => void controller.interrupt()}
					>
						{controller.interrupting ? '中断中…' : '中断当前回合'}
					</button>
				{/if}
				{#if session}
					<button
						class="btn"
						type="button"
						disabled={shareBusy}
						onclick={() => void share({ scope: 'session' })}
					>
						{shareBusy ? '生成链接中…' : '分享整段会话'}
					</button>
					<div class="panel-switch" role="group" aria-label="侧边面板">
						<button
							class="btn"
							type="button"
							aria-pressed={panel === 'files'}
							onclick={() => togglePanel('files')}
						>
							{panel === 'files' ? '收起文件' : '文件'}
						</button>
						<button
							class="btn"
							type="button"
							aria-pressed={panel === 'terminal'}
							onclick={() => togglePanel('terminal')}
						>
							{panel === 'terminal' ? '收起终端' : '终端'}
						</button>
					</div>
				{/if}
			</div>
		</header>

		{#if controller.error}
			<p class="alert banner">{controller.error}</p>
		{/if}

		{#if shareError}
			<p class="alert banner" role="alert">分享链接未创建：{shareError}</p>
		{/if}

		{#if shareLink}
			<ShareLinkCard
				url={absoluteHref({ page: 'share', shareId: shareLink.id })}
				label={shareLabel}
				createdAt={shareLink.createdAt}
				ondismiss={() => (shareLink = null)}
			/>
		{/if}

		{#if controller.runtimeState?.statusReason}
			<p class="banner notice-line">状态原因：{controller.runtimeState.statusReason}</p>
		{/if}

		{#if controller.interruptError}
			<p class="alert banner" role="alert">中断未下发：{controller.interruptError}</p>
		{:else if controller.interruptStream && canInterrupt}
			<p class="banner notice-line">
				中断指令已下发（stream <code class="mono">{controller.interruptStream}</code>），等待运行时回推状态更新。
			</p>
		{/if}

		{#if showApprovals}
			<section class="approvals" aria-label="审批请求">
				{#each controller.approvalNotices as notice (notice.id)}
					<ApprovalCard
						{notice}
						action={controller.noticeAction(notice.id)}
						onRespond={(decision) => void controller.respond(notice.id, decision)}
					/>
				{/each}
				{#each controller.pendingApprovalItems as item (item.id)}
					<article class="approval pending-item" data-item-id={item.id}>
						<div class="pending-head">
							<span class="badge" data-tone="warn">等待审批</span>
							<span class="pending-title">{itemHeadline(item)}</span>
						</div>
						<p class="pending-hint">
							时间线条目 <code class="mono">{item.id}</code> 处于等待审批，但 hub 尚未下发对应的审批详情（notice），因此这里没有可提交的决定。
						</p>
					</article>
				{/each}
			</section>
		{/if}

		<div class="timeline" bind:this={scroller} onscroll={handleScroll}>
			{#if controller.loading}
				<div class="empty"><p>正在加载会话快照…</p></div>
			{:else if controller.visibleItems.length === 0 && controller.optimistic.length === 0}
				<div class="empty">
					<p>这个会话还没有时间线内容。</p>
					<p>在下方输入框发送第一条消息。</p>
				</div>
			{:else}
				{#each controller.visibleItems as item (item.id)}
					<TimelineEntry
						{item}
						onshare={(shared) => void share({ scope: 'message', messageId: shared.id })}
					/>
				{/each}
			{/if}

			{#each controller.optimistic as row (row.clientMessageId)}
				<article class="optimistic" class:failed={row.failed} data-type="message">
					<header>
						<span class="kind">
							<span class="kind-label">消息</span>
							<span class="role">用户</span>
						</span>
						<span class="right">
							{#if row.failed}
								<span class="badge" data-tone="err">发送失败</span>
							{:else}
								<span class="badge" data-tone="warn">发送中…</span>
							{/if}
							<span class="time">{formatClock(row.createdAt)}</span>
						</span>
					</header>
					<p class="body">{row.content}</p>
					{#if row.failed && row.error}
						<p class="alert">{row.error}</p>
						<button class="btn" type="button" onclick={() => controller.discardOptimistic(row.clientMessageId)}>
							移除这条
						</button>
					{/if}
				</article>
			{/each}
		</div>

		<form class="composer" onsubmit={(event) => { event.preventDefault(); void submit(); }}>
			<textarea
				class="input"
				rows="2"
				placeholder="输入消息，Enter 发送，Shift+Enter 换行"
				bind:value={draft}
				onkeydown={handleKeydown}
				disabled={controller.session === null}
			></textarea>
			<div class="composer-actions">
				<span class="hint">消息经 <code class="mono">POST /runtime/messages</code> 发送，随后由 WS 增量更新</span>
				<button
					class="btn"
					data-variant="primary"
					type="submit"
					disabled={draft.trim().length === 0 || controller.session === null}
				>
					发送
				</button>
			</div>
		</form>
	</section>

	{#if panel === 'files' && session}
		<FilePanel
			{token}
			connectorId={session.connectorId}
			workspacePath={workspacePath}
			{connectorOnline}
		/>
	{:else if panel === 'terminal' && session}
		<TerminalPanel
			{token}
			connectorId={session.connectorId}
			cwd={workspacePath}
			{connectorOnline}
		/>
	{/if}
</div>

<style>
	.session-layout {
		display: flex;
		height: 100vh;
		min-height: 0;
	}

	.session {
		display: flex;
		flex-direction: column;
		flex: 1;
		min-width: 0;
		height: 100%;
		min-height: 0;
	}

	header {
		display: flex;
		align-items: flex-start;
		gap: 12px;
		padding: 14px 20px;
		border-bottom: 1px solid var(--border);
		background: oklch(0.17 0 0);
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

	.title-block {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 140px;
		flex: 1;
	}

	h1 {
		font-size: 15px;
		letter-spacing: -0.01em;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.meta {
		display: flex;
		gap: 6px;
		font-size: 10.5px;
		color: var(--muted-foreground);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.badges {
		display: flex;
		align-items: center;
		justify-content: flex-end;
		flex-wrap: wrap;
		gap: 6px;
		min-width: 0;
		flex: 0 1 auto;
	}

	.badges button {
		flex: none;
		white-space: nowrap;
	}

	.banner {
		margin: 12px 20px 0;
	}

	.notice-line {
		padding: 8px 12px;
		border-radius: var(--radius-md);
		border: 1px solid color-mix(in oklch, var(--info) 40%, transparent);
		background: color-mix(in oklch, var(--info) 10%, transparent);
		color: oklch(0.9 0.03 240);
		font-size: 12px;
	}

	.status-alert {
		font-weight: 700;
		box-shadow: 0 0 0 3px color-mix(in oklch, var(--warn) 22%, transparent);
	}

	.approvals {
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 12px 20px 4px;
		max-height: 42vh;
		overflow-y: auto;
		border-bottom: 1px solid var(--border);
	}

	.pending-item {
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 10px 12px;
		border-radius: var(--radius-md);
		border: 1px dashed color-mix(in oklch, var(--warn) 40%, transparent);
		background: color-mix(in oklch, var(--warn) 6%, transparent);
	}

	.pending-head {
		display: flex;
		align-items: center;
		gap: 8px;
		min-width: 0;
	}

	.pending-title {
		font-size: 12.5px;
		font-weight: 500;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.pending-hint {
		font-size: 11px;
		color: var(--muted-foreground);
		line-height: 1.6;
	}

	.timeline {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 16px 20px 24px;
	}

	.optimistic {
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 10px 12px;
		border-radius: var(--radius-md);
		border: 1px dashed color-mix(in oklch, var(--info) 40%, transparent);
		background: color-mix(in oklch, var(--info) 8%, transparent);
		opacity: 0.85;
	}

	.optimistic.failed {
		border-color: color-mix(in oklch, var(--err) 45%, transparent);
		background: color-mix(in oklch, var(--err) 10%, transparent);
	}

	.optimistic header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
		padding: 0;
		border: none;
		background: none;
	}

	.kind {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.kind-label {
		font-size: 10.5px;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--muted-foreground);
	}

	.role {
		font-size: 11.5px;
		font-weight: 500;
	}

	.right {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.time {
		font-size: 10.5px;
		color: var(--muted-foreground);
		font-variant-numeric: tabular-nums;
	}

	.body {
		margin: 0;
		font-size: 13px;
		font-weight: 400;
		line-height: 1.65;
		white-space: pre-wrap;
		word-break: break-word;
	}

	.composer {
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 12px 20px 16px;
		border-top: 1px solid var(--border);
		background: oklch(0.17 0 0);
	}

	textarea.input {
		height: auto;
		padding: 10px 12px;
		resize: vertical;
		min-height: 60px;
		line-height: 1.6;
	}

	.composer-actions {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}

	.hint {
		font-size: 10.5px;
		color: var(--muted-foreground);
	}

	code {
		font-size: 10px;
	}
	.panel-switch {
		display: flex;
		gap: 4px;
	}

	.panel-switch .btn[aria-pressed='true'] {
		border-color: color-mix(in oklch, var(--info) 45%, transparent);
		color: var(--info);
	}

</style>
