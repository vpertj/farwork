<script lang="ts">
	import { workspace } from '$lib/workspace.svelte';
	import { formatRelative } from '$lib/format';
	import { runtimeLabel, sessionStatusLabel, sessionStatusTone } from '$lib/timeline';

	const recent = $derived(
		[...workspace.sessions]
			.filter((session) => !session.archived)
			.sort((a, b) => {
				const left = a.lastActivityAt ? Date.parse(a.lastActivityAt) : 0;
				const right = b.lastActivityAt ? Date.parse(b.lastActivityAt) : 0;
				return right - left;
			})
			.slice(0, 8)
	);
</script>

<section class="task">
	<header>
		<div>
			<h1>任务台</h1>
			<p>按设备查看项目与会话；点开会话即可对话、查看时间线。</p>
		</div>
	</header>

	<div class="stats">
		<div class="stat">
			<span class="label">设备</span>
			<strong>{workspace.connectors.length}</strong>
			<span class="hint">{workspace.onlineDevices} 在线</span>
		</div>
		<div class="stat">
			<span class="label">项目</span>
			<strong>{workspace.projects.length}</strong>
			<span class="hint">按连接器归属</span>
		</div>
		<div class="stat">
			<span class="label">会话</span>
			<strong>{workspace.activeSessionCount}</strong>
			<span class="hint">未归档</span>
		</div>
	</div>

	<div class="recent">
		<h2>最近会话</h2>
		{#if recent.length === 0}
			<div class="empty">
				<p>还没有任何会话。</p>
				<p>上游侧创建会话后，这里会自动出现。</p>
			</div>
		{:else}
			<ul>
				{#each recent as session (session.id)}
					<li>
						<a href="#/session/{encodeURIComponent(session.id)}">
							<div class="title-row">
								<span class="title">{session.title ?? '未命名会话'}</span>
								<span class="badge" data-tone={sessionStatusTone(session.status)}>
									<span class="dot"></span>
									{sessionStatusLabel(session.status)}
								</span>
							</div>
							<div class="meta mono">
								<span>{runtimeLabel(session.runtime)}</span>
								<span>·</span>
								<span>{session.id}</span>
								<span>·</span>
								<span>{formatRelative(session.lastActivityAt)}</span>
							</div>
						</a>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
</section>

<style>
	.task {
		display: flex;
		flex-direction: column;
		gap: 22px;
		padding: 28px 32px;
	}

	header h1 {
		font-size: 22px;
		letter-spacing: -0.01em;
	}

	header p {
		margin-top: 6px;
		font-size: 13px;
		color: var(--muted-foreground);
		font-weight: 400;
	}

	.stats {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
		gap: 12px;
	}

	.stat {
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding: 14px 16px;
		border-radius: var(--radius-lg);
		border: 1px solid var(--border);
		background: oklch(1 0 0 / 3%);
	}

	.stat .label {
		font-size: 11px;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		color: var(--muted-foreground);
	}

	.stat strong {
		font-size: 24px;
		font-variant-numeric: tabular-nums;
		font-weight: 600;
	}

	.stat .hint {
		font-size: 11.5px;
		color: var(--muted-foreground);
	}

	.recent {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	h2 {
		font-size: 13px;
		color: var(--muted-foreground);
		font-weight: 500;
		letter-spacing: 0.04em;
	}

	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	li a {
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 12px 14px;
		border-radius: var(--radius-md);
		border: 1px solid transparent;
		background: oklch(1 0 0 / 2.5%);
		transition: background 120ms ease, border-color 120ms ease;
	}

	li a:hover {
		background: oklch(1 0 0 / 6%);
		border-color: var(--border);
	}

	.title-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}

	.title {
		font-size: 13.5px;
		font-weight: 500;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.meta {
		display: flex;
		gap: 6px;
		font-size: 11px;
		color: var(--muted-foreground);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
</style>
