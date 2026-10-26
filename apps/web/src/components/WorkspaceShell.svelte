<script lang="ts">
	import type { Snippet } from 'svelte';
	import Sidebar from './Sidebar.svelte';
	import type { Route } from '$lib/router';

	let { route, children }: { route: Route; children: Snippet } = $props();
</script>

<div class="shell">
	<Sidebar {route} />
	<main>
		{@render children()}
	</main>
</div>

<style>
	.shell {
		display: flex;
		height: 100vh;
		overflow: hidden;
		background: var(--background);
	}

	main {
		flex: 1;
		min-width: 0;
		height: 100vh;
		overflow: hidden;
		display: flex;
		flex-direction: column;
	}
</style>
