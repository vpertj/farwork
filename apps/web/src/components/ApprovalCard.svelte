<script lang="ts">
	import type { NoticeDecision, RuntimeNotice } from '$lib/api/types';
	import type { NoticeAction } from '$lib/session.svelte';
	import { formatClock } from '$lib/format';
	import {
		noticeDecisionLabel,
		noticeFields,
		noticeKindLabel,
		noticeKindTone
	} from '$lib/notice';

	let {
		notice,
		action,
		onRespond
	}: {
		notice: RuntimeNotice;
		action: NoticeAction;
		onRespond: (decision: NoticeDecision) => void;
	} = $props();

	const resolved = $derived(notice.status === 'resolved');
	const fields = $derived(noticeFields(notice.body));
	const awaitingRuntime = $derived(!resolved && (action.pending || action.submitted !== null));
	const buttonsDisabled = $derived(resolved || action.pending || action.submitted !== null);

	const statusText = $derived(
		resolved
			? noticeDecisionLabel(notice.decision)
			: action.pending
				? '提交中…'
				: action.submitted
					? `已下发「${noticeDecisionLabel(action.submitted)}」，等待运行时确认`
					: '待审批'
	);

	function retry(): void {
		onRespond(action.lastDecision ?? 'approve');
	}
</script>

<article class="approval" class:resolved data-notice-id={notice.id} data-kind={notice.kind}>
	<header>
		<div class="titles">
			<div class="kind-row">
				<span class="badge" data-tone={noticeKindTone(notice.kind)}>
					{noticeKindLabel(notice.kind)}
				</span>
				<span class="badge" data-tone={resolved ? 'muted' : 'warn'}>{statusText}</span>
			</div>
			<h3>{notice.title || '未命名审批请求'}</h3>
		</div>
		<span class="time">{formatClock(notice.createdAt)}</span>
	</header>

	<dl class="fields">
		{#each fields as field (field.label)}
			<div class="field-row">
				<dt>{field.label}</dt>
				<dd class:mono={field.mono}>{field.value}</dd>
			</div>
		{/each}
	</dl>

	{#if action.error}
		<p class="alert" role="alert">{action.error}</p>
	{/if}

	{#if !resolved}
		<div class="actions">
			<button
				class="btn"
				data-variant="primary"
				type="button"
				disabled={buttonsDisabled}
				onclick={() => onRespond('approve')}
			>
				批准
			</button>
			<button
				class="btn"
				type="button"
				disabled={buttonsDisabled}
				onclick={() => onRespond('approve_for_session')}
			>
				本会话内总是批准
			</button>
			<button
				class="btn"
				type="button"
				disabled={buttonsDisabled}
				onclick={() => onRespond('reject')}
			>
				拒绝
			</button>
			{#if action.error && !action.pending}
				<button class="btn" data-variant="ghost" type="button" onclick={retry}>重试</button>
			{/if}
		</div>
		{#if awaitingRuntime}
			<p class="hint">决定已送达 hub；卡片状态以运行时回推的 notice.resolved 为准。</p>
		{:else}
			<p class="hint">决定经 <code class="mono">POST /notices/{notice.id}/respond</code> 下发到设备。</p>
		{/if}
	{:else}
		<p class="hint">运行时已确认：{noticeDecisionLabel(notice.decision)}</p>
	{/if}
</article>

<style>
	.approval {
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 12px 14px;
		border-radius: var(--radius-md);
		border: 1px solid color-mix(in oklch, var(--warn) 45%, transparent);
		background: color-mix(in oklch, var(--warn) 9%, transparent);
	}

	.approval.resolved {
		border-color: var(--border);
		background: oklch(1 0 0 / 3%);
		opacity: 0.72;
	}

	header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 10px;
	}

	.titles {
		display: flex;
		flex-direction: column;
		gap: 6px;
		min-width: 0;
	}

	.kind-row {
		display: flex;
		align-items: center;
		gap: 6px;
	}

	h3 {
		font-size: 13.5px;
		font-weight: 600;
		line-height: 1.5;
		word-break: break-word;
	}

	.time {
		font-size: 10.5px;
		color: var(--muted-foreground);
		font-variant-numeric: tabular-nums;
		flex: none;
	}

	.fields {
		display: flex;
		flex-direction: column;
		gap: 4px;
		margin: 0;
	}

	.field-row {
		display: grid;
		grid-template-columns: 72px 1fr;
		gap: 8px;
		align-items: start;
	}

	dt {
		font-size: 11px;
		color: var(--muted-foreground);
		padding-top: 1px;
	}

	dd {
		margin: 0;
		font-size: 12px;
		line-height: 1.55;
		white-space: pre-wrap;
		word-break: break-word;
	}

	dd.mono {
		font-size: 11.5px;
		padding: 4px 6px;
		border-radius: var(--radius-sm);
		background: oklch(0.1 0 0 / 70%);
		border: 1px solid var(--border);
		max-height: 180px;
		overflow: auto;
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}

	.hint {
		font-size: 10.5px;
		color: var(--muted-foreground);
	}

	code {
		font-size: 10px;
	}
</style>
