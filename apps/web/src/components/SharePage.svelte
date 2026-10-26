<script lang="ts">
	import { ApiError } from '$lib/api/client';
	import { fetchPublicShare } from '$lib/api/shares';
	import type { PublicShare } from '$lib/api/types';
	import { describeError, formatModified } from '$lib/format';
	import { isVisibleItem, sortItems } from '$lib/timeline';
	import TimelineEntry from './TimelineEntry.svelte';

	let { shareId }: { shareId: string } = $props();

	type ShareState =
		| { kind: 'loading' }
		| { kind: 'ready'; share: PublicShare }
		| { kind: 'missing' }
		| { kind: 'error'; message: string };

	let state = $state<ShareState>({ kind: 'loading' });
	let loadedId = '';

	const share = $derived(state.kind === 'ready' ? state.share : null);
	const items = $derived(share ? sortItems(share.items.filter(isVisibleItem)) : []);
	const scopeLabel = $derived(share?.scope === 'message' ? '单条消息' : '整段会话');
	const heading = $derived(share && share.sessionTitle.trim().length > 0 ? share.sessionTitle : '会话分享');

	async function load(id: string): Promise<void> {
		if (id.length === 0) {
			state = { kind: 'missing' };
			return;
		}
		state = { kind: 'loading' };
		try {
			state = { kind: 'ready', share: await fetchPublicShare(id) };
		} catch (error) {
			if (error instanceof ApiError && (error.status === 404 || error.status === 410)) {
				state = { kind: 'missing' };
				return;
			}
			state = { kind: 'error', message: describeError(error) };
		}
	}

	$effect(() => {
		const id = shareId;
		if (loadedId === id) return;
		loadedId = id;
		void load(id);
	});

	function retry(): void {
		loadedId = '';
		void load(shareId);
	}
</script>

<div class="public-page">
	<header class="topbar">
		<span class="glyph" aria-hidden="true">◈</span>
		<div class="brand">
			<span class="brand-name">Farwork</span>
			<span class="badge">只读分享</span>
		</div>
		{#if share}
			<span class="badge" data-tone="info">{scopeLabel}</span>
		{/if}
	</header>

	<main>
		{#if state.kind === 'loading'}
			<div class="empty"><p>正在加载分享内容…</p></div>
		{:else if state.kind === 'missing'}
			<div class="empty">
				<p class="headline">分享不存在或已失效</p>
				<p>这条分享链接可能已被撤销，或链接本身不完整。</p>
			</div>
		{:else if state.kind === 'error'}
			<div class="empty">
				<p class="headline">分享内容加载失败</p>
				<p class="mono message">{state.message}</p>
				<button class="btn" type="button" onclick={retry}>重试</button>
			</div>
		{:else}
			<section class="intro">
				<h1>{heading}</h1>
				<div class="meta mono">
					<span>分享于 {formatModified(state.share.createdAt)}</span>
					<span>·</span>
					<span>{scopeLabel}</span>
					<span>·</span>
					<span>{items.length} 条时间线</span>
				</div>
			</section>

			{#if items.length === 0}
				<div class="empty"><p>这条分享没有可展示的时间线内容。</p></div>
			{:else}
				<div class="timeline">
					{#each items as item (item.id)}
						<TimelineEntry {item} />
					{/each}
				</div>
			{/if}
		{/if}
	</main>

	<footer>
		<p>此页面为公开只读分享，无需登录即可查看。</p>
	</footer>
</div>

<style>
	.public-page {
		min-height: 100vh;
		display: flex;
		flex-direction: column;
		background: var(--background);
		color: var(--foreground);
	}

	.topbar {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 20px;
		border-bottom: 1px solid var(--border);
		background: oklch(0.17 0 0);
	}

	.glyph {
		font-size: 16px;
		color: var(--info);
	}

	.brand {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.brand-name {
		font-size: 13px;
		font-weight: 600;
		letter-spacing: -0.01em;
	}

	main {
		flex: 1;
		width: 100%;
		max-width: 880px;
		margin: 0 auto;
		padding: 20px 20px 40px;
		display: flex;
		flex-direction: column;
		gap: 14px;
	}

	.intro {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	h1 {
		font-size: 18px;
		letter-spacing: -0.01em;
		line-height: 1.4;
		overflow-wrap: anywhere;
	}

	.meta {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
		font-size: 11px;
		color: var(--muted-foreground);
	}

	.timeline {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.headline {
		font-size: 15px;
		font-weight: 600;
		color: var(--foreground);
	}

	.message {
		font-size: 11.5px;
		max-width: 520px;
		overflow-wrap: anywhere;
	}

	footer {
		padding: 14px 20px 20px;
		border-top: 1px solid var(--border);
		color: var(--muted-foreground);
		font-size: 11px;
		text-align: center;
	}

	footer p {
		margin: 0;
	}
</style>
