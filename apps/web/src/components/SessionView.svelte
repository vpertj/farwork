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
	import {
		commandDraft,
		matchCommands,
		slashQuery,
		stepCommandIndex
	} from '$lib/commands';
	import ApprovalCard from './ApprovalCard.svelte';
	import CwdLabel from './CwdLabel.svelte';
	import FilePanel from './FilePanel.svelte';
	import TerminalPanel from './TerminalPanel.svelte';
	import TimelineEntry from './TimelineEntry.svelte';
	import ShareLinkCard from './ShareLinkCard.svelte';
	import SlashCommandMenu from './SlashCommandMenu.svelte';

	let { sessionId, token }: { sessionId: string; token: string } = $props();

	const controller = new SessionController();
	let draft = $state('');
	let steerDraft = $state('');
	let dismissedQuery = $state<string | null>(null);
	let activeSuggestion = $state(0);
	let scroller: HTMLDivElement | null = $state(null);
	let stickToBottom = true;

	const session = $derived(controller.session);
	const status = $derived(controller.runtimeState?.status ?? session?.status ?? 'idle');
	const flags = $derived(controller.flags);
	const modelOptions = $derived(controller.catalogs.models);
	const permissionOptions = $derived(controller.catalogs.permissions);
	const commands = $derived(controller.commands);
	const canInterrupt = $derived(
		flags.interrupt && (status === 'running' || status === 'waiting_approval')
	);
	const canSteer = $derived(flags.steer && status === 'running');
	const slash = $derived(flags.commands && commands.length > 0 ? slashQuery(draft) : null);
	const suggestions = $derived(
		slash !== null && slash !== dismissedQuery ? matchCommands(commands, slash) : []
	);
	const activeIndex = $derived(
		suggestions.length === 0 ? -1 : Math.min(activeSuggestion, suggestions.length - 1)
	);

	$effect(() => {
		void slash;
		activeSuggestion = 0;
	});

	const showApprovals = $derived(
		controller.approvalNotices.length > 0 || controller.pendingApprovalItems.length > 0
	);
	const project = $derived(workspace.projectById(session?.projectId ?? null));
	const workspacePath = $derived(project?.workspacePath ?? session?.cwd ?? '');
	const connectorOnline = $derived((session?.connectorStatus ?? 'offline') === 'online');

	// 能力是运行时启动后才上报的，页面可能先加载：每次进入 running/waiting_approval
	// 再读一次，避免一直用「还没上报」时的全 false 判据。只跟状态变化，不轮询。
	let capabilityStatus = '';
	$effect(() => {
		const current = status;
		if (capabilityStatus === current) return;
		const first = capabilityStatus === '';
		capabilityStatus = current;
		if (!first && (current === 'running' || current === 'waiting_approval')) {
			void controller.loadCapabilities();
		}
	});

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

	async function submitSteer(): Promise<void> {
		const text = steerDraft.trim();
		if (text.length === 0) return;
		const outcome = await controller.steer(text);
		if (outcome.ok) steerDraft = '';
	}

	/** 把没投递出去的补充指令搬回普通消息输入区，用户不用重打一遍。 */
	function promoteSteer(): void {
		const text = steerDraft.trim();
		if (text.length === 0) return;
		draft = text;
		steerDraft = '';
		controller.clearSteerMessages();
	}

	function pickCommand(name: string): void {
		draft = commandDraft(name);
		dismissedQuery = null;
		activeSuggestion = 0;
	}

	function handleKeydown(event: KeyboardEvent): void {
		if (suggestions.length > 0) {
			if (event.key === 'ArrowDown') {
				event.preventDefault();
				activeSuggestion = stepCommandIndex(activeSuggestion, 1, suggestions.length);
				return;
			}
			if (event.key === 'ArrowUp') {
				event.preventDefault();
				activeSuggestion = stepCommandIndex(activeSuggestion, -1, suggestions.length);
				return;
			}
			if (event.key === 'Escape') {
				event.preventDefault();
				dismissedQuery = slash;
				return;
			}
			if (event.key === 'Enter' || event.key === 'Tab') {
				const picked = suggestions[activeIndex];
				if (picked) {
					event.preventDefault();
					pickCommand(picked.name);
					return;
				}
			}
		}
		if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
			event.preventDefault();
			void submit();
		}
	}

	function handleSteerKeydown(event: KeyboardEvent): void {
		if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
			event.preventDefault();
			void submitSteer();
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
				<div class="cwd-line" data-testid="session-cwd">
					<CwdLabel cwd={session?.cwd ?? null} label="工作目录" maxLength={72} />
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

		{#if controller.capabilitiesError}
			<p class="alert banner" role="alert">
				运行时能力未取得：{controller.capabilitiesError}
				{#if !controller.capabilitiesLoading}
					<button class="btn" type="button" onclick={() => void controller.loadCapabilities()}>
						重新读取能力
					</button>
				{/if}
			</p>
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
						onRespond={(actionId, inputData) =>
							void controller.respond(notice.id, actionId, inputData)}
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

		<div class="dock">
			{#if flags.selections}
				<div class="runtime-bar" aria-label="运行时选择">
					{#if modelOptions.length > 0}
						<label class="field compact">
							<span>模型</span>
							{#key controller.selectionsRevision}
								<select
									class="input"
									value={controller.selections.model ?? ''}
									disabled={controller.selectionsSaving}
									onchange={(event) => void controller.selectModel(event.currentTarget.value)}
								>
									<option value="">未选择</option>
									{#each modelOptions as option (option.id)}
										<option value={option.id} title={option.description}>{option.name}</option>
									{/each}
								</select>
							{/key}
						</label>
					{/if}
					{#if permissionOptions.length > 0}
						<label class="field compact">
							<span>权限模式</span>
							{#key controller.selectionsRevision}
								<select
									class="input"
									value={controller.selections.permissionMode ?? ''}
									disabled={controller.selectionsSaving}
									onchange={(event) => void controller.selectPermission(event.currentTarget.value)}
								>
									<option value="">未选择</option>
									{#each permissionOptions as option (option.id)}
										<option value={option.id} title={option.description}>{option.name}</option>
									{/each}
								</select>
							{/key}
						</label>
					{/if}
					{#if modelOptions.length === 0 && permissionOptions.length === 0}
						{#if !controller.listsError}
							<span class="hint">服务端没有给出模型/权限目录，选择器无处可渲染。</span>
						{/if}
					{:else if controller.selectionsSaving}
						<span class="hint">正在等服务端确认…</span>
					{/if}
				</div>
			{/if}

			{#if controller.selectionsError}
				<p class="alert compact" role="alert">切换未生效：{controller.selectionsError}</p>
			{/if}

			{#if controller.listsError}
				<p class="alert compact" role="alert">运行时目录读取失败：{controller.listsError}</p>
			{/if}

			{#if controller.steerError}
				<p class="alert compact" role="alert">
					{controller.steerError}
					{#if steerDraft.trim().length > 0}
						<button class="btn" type="button" onclick={promoteSteer}>改为发送普通消息</button>
					{/if}
				</p>
			{:else if controller.steerNotice}
				<p class="steer-note">{controller.steerNotice}</p>
			{/if}

			{#if canSteer}
				<form
					class="steer"
					onsubmit={(event) => {
						event.preventDefault();
						void submitSteer();
					}}
				>
					<div class="steer-head">
						<span class="badge" data-tone="info">补充指令</span>
						<span class="hint">
							追加给正在运行的回合，走 <code class="mono">POST /runtime/steer</code>，不新增用户消息
						</span>
					</div>
					<textarea
						class="input"
						rows="2"
						placeholder="补充给当前回合的指令，Enter 投递，Shift+Enter 换行"
						bind:value={steerDraft}
						onkeydown={handleSteerKeydown}
						disabled={controller.steering}
					></textarea>
					<div class="composer-actions">
						<span class="hint">会话 state=running 且有 steer 能力时才出现</span>
						<button
							class="btn"
							type="submit"
							disabled={steerDraft.trim().length === 0 || controller.steering}
						>
							{controller.steering ? '投递中…' : '投递补充指令'}
						</button>
					</div>
				</form>
			{/if}

			<form
				class="composer"
				onsubmit={(event) => {
					event.preventDefault();
					void submit();
				}}
			>
				<div class="composer-input">
					{#if suggestions.length > 0}
						<div class="suggestions">
							<SlashCommandMenu items={suggestions} {activeIndex} onpick={pickCommand} />
						</div>
					{/if}
					<textarea
						class="input"
						rows="2"
						placeholder="输入消息，Enter 发送，Shift+Enter 换行"
						bind:value={draft}
						onkeydown={handleKeydown}
						disabled={controller.session === null}
					></textarea>
				</div>
				<div class="composer-actions">
					<span class="hint">
						消息经 <code class="mono">POST /runtime/messages</code> 发送，随后由 WS 增量更新
						{#if flags.commands && commands.length > 0}
							· 输入 <code class="mono">/</code> 选择命令（{commands.length} 条），选中只填入文本
						{/if}
					</span>
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
		</div>
	</section>

	{#if panel === 'files' && session}
		<FilePanel {token} {sessionId} workspacePath={workspacePath} {connectorOnline} />
	{:else if panel === 'terminal' && session}
		<TerminalPanel {token} {sessionId} cwd={workspacePath} {connectorOnline} />
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

	.cwd-line {
		display: flex;
		align-items: center;
		gap: 6px;
		margin-top: 4px;
		min-width: 0;
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

	.dock {
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 12px 20px 16px;
		border-top: 1px solid var(--border);
		background: oklch(0.17 0 0);
	}

	.composer,
	.steer {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.composer-input {
		position: relative;
	}

	.suggestions {
		position: absolute;
		bottom: calc(100% + 6px);
		left: 0;
		right: 0;
		z-index: 5;
	}

	.runtime-bar {
		display: flex;
		align-items: center;
		gap: 12px;
		flex-wrap: wrap;
	}

	.field.compact {
		flex-direction: row;
		align-items: center;
		gap: 6px;
	}

	.field.compact > span {
		font-size: 11px;
		white-space: nowrap;
	}

	.field.compact select.input {
		height: 30px;
		padding: 0 8px;
		font-size: 12px;
		min-width: 150px;
	}

	.alert.compact {
		display: flex;
		align-items: center;
		gap: 10px;
		flex-wrap: wrap;
		padding: 7px 10px;
		font-size: 12px;
	}

	.steer {
		padding: 8px 10px;
		border-radius: var(--radius-md);
		border: 1px dashed color-mix(in oklch, var(--info) 45%, transparent);
		background: color-mix(in oklch, var(--info) 7%, transparent);
	}

	.steer-head {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-wrap: wrap;
	}

	.steer textarea.input {
		min-height: 46px;
		padding: 8px 10px;
		resize: vertical;
	}

	.steer-note {
		padding: 7px 10px;
		border-radius: var(--radius-md);
		border: 1px solid color-mix(in oklch, var(--info) 40%, transparent);
		background: color-mix(in oklch, var(--info) 10%, transparent);
		color: oklch(0.9 0.03 240);
		font-size: 12px;
		line-height: 1.5;
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