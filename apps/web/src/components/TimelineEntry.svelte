<script lang="ts">
	import type { TimelineItem } from '$lib/api/types';
	import { formatClock } from '$lib/format';
	import {
		itemBody,
		itemHeadline,
		itemStatusLabel,
		itemStatusTone,
		itemTypeLabel,
		isCollapsible
	} from '$lib/timeline';

	let { item, onshare }: { item: TimelineItem; onshare?: (item: TimelineItem) => void } =
		$props();

	let expanded = $state(false);

	const role = $derived(item.role ?? '');
	const isUserMessage = $derived(item.type === 'message' && role === 'user');
	const heading = $derived(itemHeadline(item));
	const body = $derived(itemBody(item));
	const collapsible = $derived(isCollapsible(item));
	const preview = $derived(body.length > 220 ? `${body.slice(0, 220)}…` : body);
</script>

<article class="entry" data-type={item.type} class:user={isUserMessage}>
	<header>
		<span class="kind">
			<span class="kind-label">{itemTypeLabel(item.type)}</span>
			{#if item.type === 'message' && role}
				<span class="role">{role === 'user' ? '用户' : role === 'assistant' ? '助手' : role}</span>
			{/if}
			{#if collapsible}
				<span class="headline mono">{heading}</span>
			{/if}
		</span>
		<span class="right">
			{#if item.status !== 'done'}
				<span class="badge" data-tone={itemStatusTone(item.status)}>
					{itemStatusLabel(item.status)}
				</span>
			{/if}
			{#if onshare}
				<button
					class="share"
					type="button"
					title="分享这条消息"
					onclick={() => onshare?.(item)}
				>
					分享
				</button>
			{/if}
			<span class="time">{formatClock(item.createdAt)}</span>
		</span>
	</header>

	{#if body}
		{#if collapsible}
			<button class="toggle" type="button" onclick={() => (expanded = !expanded)}>
				{expanded ? '收起详情' : '展开详情'}
			</button>
			<pre class="body mono">{expanded ? body : preview}</pre>
		{:else}
			<p class="body">{body}</p>
		{/if}
	{/if}
</article>

<style>
	.entry {
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 10px 12px;
		border-radius: var(--radius-md);
		border: 1px solid transparent;
		background: oklch(1 0 0 / 2%);
	}

	.entry[data-type='tool'],
	.entry[data-type='artifact'] {
		border-color: var(--border);
		background: oklch(1 0 0 / 3%);
	}

	.entry.user {
		border-color: color-mix(in oklch, var(--info) 30%, transparent);
		background: color-mix(in oklch, var(--info) 8%, transparent);
	}

	header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
	}

	.kind {
		display: flex;
		align-items: center;
		gap: 8px;
		min-width: 0;
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

	.headline {
		font-size: 11.5px;
		color: var(--muted-foreground);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.right {
		display: flex;
		align-items: center;
		gap: 8px;
		flex: none;
	}

	.time {
		font-size: 10.5px;
		color: var(--muted-foreground);
		font-variant-numeric: tabular-nums;
	}

	.body {
		font-size: 13px;
		font-weight: 400;
		line-height: 1.65;
		white-space: pre-wrap;
		word-break: break-word;
		margin: 0;
		color: var(--foreground);
	}

	pre.body {
		font-size: 11.5px;
		padding: 8px 10px;
		border-radius: var(--radius-sm);
		background: oklch(0.1 0 0);
		border: 1px solid var(--border);
		overflow-x: auto;
		max-height: 420px;
	}

	.toggle {
		align-self: flex-start;
		font-size: 11px;
		color: var(--muted-foreground);
		text-decoration: underline;
		text-underline-offset: 2px;
	}

	.toggle:hover {
		color: var(--foreground);
	}

	.share {
		padding: 1px 7px;
		border-radius: 999px;
		border: 1px solid var(--border);
		font-size: 10.5px;
		color: var(--muted-foreground);
	}

	.share:hover {
		border-color: color-mix(in oklch, var(--info) 45%, transparent);
		color: var(--info);
	}
</style>
