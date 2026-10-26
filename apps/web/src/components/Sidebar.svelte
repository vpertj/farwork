<script lang="ts">
	import { auth } from '$lib/auth.svelte';
	import { workspace } from '$lib/workspace.svelte';
	import { formatRelative } from '$lib/format';
	import { sessionStatusLabel, sessionStatusTone } from '$lib/timeline';
	import type { Route } from '$lib/router';

	let { route }: { route: Route } = $props();

	const activeSessionId = $derived(route.page === 'session' ? route.sessionId : null);

	function signOut(): void {
		auth.signOut();
		window.location.hash = '#/login';
	}
</script>

<aside class="sidebar">
	<div class="head">
		<a class="brand" href="#/">
			<span class="glyph" aria-hidden="true">◈</span>
			<span>Farwork</span>
		</a>
		<button
			class="btn"
			data-variant="ghost"
			type="button"
			title="刷新设备与会话"
			onclick={() => auth.token && workspace.load(auth.token)}
			disabled={workspace.loading}
		>
			{workspace.loading ? '刷新中' : '刷新'}
		</button>
	</div>

	{#if workspace.error}
		<p class="alert tight">{workspace.error}</p>
	{/if}

	<nav>
		{#if workspace.connectors.length === 0 && !workspace.loading}
			<div class="empty">
				<p>暂无设备</p>
				<p>连接器上线后会自动出现在这里。</p>
			</div>
		{/if}

		{#each workspace.groups as group (group.connector.id)}
			<section class="device">
				<a class="device-head" href="#/device/{encodeURIComponent(group.connector.id)}">
					<span class="dot" class:online={group.connector.status === 'online'}></span>
					<span class="device-name">{group.connector.name}</span>
					<span class="count">{group.sessionCount}</span>
				</a>
				{#if group.connector.deviceOs}
					<span class="device-os mono">{group.connector.deviceOs}</span>
				{/if}

				{#each group.projectGroups as projectGroup (projectGroup.project?.id ?? 'orphans')}
					<div class="project">
						<span class="project-name">
							{projectGroup.project ? projectGroup.project.name : '未归属项目'}
						</span>
						{#if projectGroup.project?.workspacePath}
							<span class="project-path mono">{projectGroup.project.workspacePath}</span>
						{/if}
						<ul>
							{#each projectGroup.sessions as session (session.id)}
								<li>
									<a
										class="session"
										class:active={activeSessionId === session.id}
										href="#/session/{encodeURIComponent(session.id)}"
									>
										<span class="dot" data-tone={sessionStatusTone(session.status)}></span>
										<span class="session-title">{session.title ?? session.id}</span>
										<span class="session-time">{formatRelative(session.lastActivityAt)}</span>
									</a>
									<span class="sr-only">{sessionStatusLabel(session.status)}</span>
								</li>
							{/each}
						</ul>
					</div>
				{/each}

				{#if group.projectGroups.length === 0}
					<p class="device-empty">该设备还没有项目或会话。</p>
				{/if}
			</section>
		{/each}
	</nav>

	<footer>
		<div class="user">
			<span class="user-name">{auth.user?.displayName ?? '已登录'}</span>
			<span class="user-mail mono">{auth.user?.email ?? ''}</span>
		</div>
		<div class="actions">
			<a class="btn" data-variant="ghost" href="#/settings/account">设置</a>
			<button class="btn" data-variant="ghost" type="button" onclick={signOut}>退出</button>
		</div>
	</footer>
</aside>

<style>
	.sidebar {
		display: flex;
		flex-direction: column;
		width: 288px;
		flex: none;
		height: 100vh;
		border-right: 1px solid var(--sidebar-border);
		background: var(--sidebar);
		color: var(--sidebar-foreground);
	}

	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: 14px 14px 10px;
	}

	.brand {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: 13px;
		font-weight: 600;
		letter-spacing: 0.06em;
		text-transform: uppercase;
	}

	.glyph {
		color: var(--info);
		font-size: 15px;
	}

	.tight {
		margin: 0 12px 8px;
	}

	nav {
		flex: 1;
		overflow-y: auto;
		padding: 4px 8px 12px;
		display: flex;
		flex-direction: column;
		gap: 14px;
	}

	.device {
		display: flex;
		flex-direction: column;
		gap: 6px;
	}

	.device-head {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 6px 8px;
		border-radius: var(--radius-sm);
		color: var(--muted-foreground);
	}

	.device-head:hover {
		background: var(--sidebar-accent);
	}

	.device-head .dot {
		background: var(--muted-foreground);
	}

	.device-head .dot.online {
		background: var(--ok);
		box-shadow: 0 0 0 3px color-mix(in oklch, var(--ok) 22%, transparent);
	}

	.device-name {
		font-size: 12px;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.count {
		margin-left: auto;
		font-size: 11px;
		font-variant-numeric: tabular-nums;
		color: var(--muted-foreground);
	}

	.device-os {
		padding: 0 8px 2px 22px;
		font-size: 10.5px;
		color: var(--muted-foreground);
		opacity: 0.75;
	}

	.project {
		display: flex;
		flex-direction: column;
		gap: 4px;
		padding-left: 8px;
	}

	.project-name {
		font-size: 12px;
		font-weight: 500;
		padding: 2px 8px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.project-path {
		padding: 0 8px 2px;
		font-size: 10.5px;
		color: var(--muted-foreground);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 1px;
	}

	.session {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 6px 8px;
		border-radius: var(--radius-sm);
		font-size: 12.5px;
		color: var(--muted-foreground);
	}

	.session:hover {
		background: var(--sidebar-accent);
		color: var(--foreground);
	}

	.session.active {
		background: var(--sidebar-accent);
		color: var(--foreground);
	}

	.session-title {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.session-time {
		font-size: 10.5px;
		opacity: 0.7;
		font-variant-numeric: tabular-nums;
	}

	[data-tone='ok'] {
		background: var(--ok);
	}

	[data-tone='warn'] {
		background: var(--warn);
	}

	[data-tone='err'] {
		background: var(--err);
	}

	[data-tone='info'] {
		background: var(--info);
	}

	[data-tone='muted'] {
		background: var(--muted-foreground);
	}

	.device-empty {
		padding: 2px 8px 6px 22px;
		font-size: 11.5px;
		color: var(--muted-foreground);
		font-weight: 400;
	}

	footer {
		border-top: 1px solid var(--sidebar-border);
		padding: 10px 12px;
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
	}

	.user {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.user-name {
		font-size: 12.5px;
		font-weight: 500;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.user-mail {
		font-size: 10.5px;
		color: var(--muted-foreground);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.actions {
		display: flex;
		gap: 2px;
	}
</style>
