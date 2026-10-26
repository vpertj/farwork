<script lang="ts">
	import { onMount } from 'svelte';
	import { auth } from '$lib/auth.svelte';
	import { workspace } from '$lib/workspace.svelte';
	import { isAuthRoute, parseHash } from '$lib/router';
	import AuthScreen from '../components/AuthScreen.svelte';
	import WorkspaceShell from '../components/WorkspaceShell.svelte';
	import TaskHome from '../components/TaskHome.svelte';
	import SessionView from '../components/SessionView.svelte';
	import NewSessionPage from '../components/NewSessionPage.svelte';
	import PlaceholderPage from '../components/PlaceholderPage.svelte';

	let hash = $state('#/');

	const route = $derived(parseHash(hash));
	const token = $derived(auth.token);

	onMount(() => {
		const sync = (): void => {
			hash = window.location.hash || '#/';
		};
		sync();
		window.addEventListener('hashchange', sync);
		void auth.load();
		return () => window.removeEventListener('hashchange', sync);
	});

	$effect(() => {
		if (!auth.ready) return;
		const current = window.location.hash || '#/';
		if (!auth.token && !isAuthRoute(route) && current !== '#/login') {
			window.location.hash = '#/login';
			return;
		}
		if (auth.token && isAuthRoute(route)) {
			window.location.hash = '#/';
		}
	});

	$effect(() => {
		if (auth.token) void workspace.load(auth.token);
	});
</script>

<svelte:head>
	<title>Farwork · 工作台</title>
</svelte:head>

{#if !auth.ready}
	<div class="boot">
		<span class="glyph" aria-hidden="true">◈</span>
		<p>正在加载工作台…</p>
	</div>
{:else if isAuthRoute(route)}
	<AuthScreen mode={route.page === 'register' ? 'register' : 'login'} />
{:else if !token}
	<div class="boot"><p>需要登录，正在跳转…</p></div>
{:else}
	<WorkspaceShell {route}>
		{#if route.page === 'home'}
			<TaskHome />
		{:else if route.page === 'session'}
			<SessionView sessionId={route.sessionId} {token} />
		{:else if route.page === 'settings'}
			<PlaceholderPage
				title={`设置 · ${route.tab}`}
				description="上游设置页包含账号、服务、通知等多个分区，本轮只打通会话链路，因此这里先给占位。"
			/>
		{:else if route.page === 'device'}
			<PlaceholderPage
				title={`设备详情 · ${route.connectorId}`}
				description="设备页需要设备运行时列表、终端与文件浏览，属于后续切片。"
			/>
		{:else if route.page === 'new-session'}
			<NewSessionPage projectId={route.projectId} {token} />
		{/if}
	</WorkspaceShell>
{/if}

<style>
	.boot {
		min-height: 100vh;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 10px;
		color: var(--muted-foreground);
		font-size: 13px;
	}

	.glyph {
		font-size: 26px;
		color: var(--info);
	}
</style>
