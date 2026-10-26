<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import type { Snippet } from 'svelte';

	let { children }: { children: Snippet } = $props();

	onMount(() => {
		if (!import.meta.env.PROD || !('serviceWorker' in navigator)) return;
		const register = () => {
			void navigator.serviceWorker.register('/sw.js').catch(() => undefined);
		};
		if (document.readyState === 'complete') register();
		else window.addEventListener('load', register, { once: true });
	});
</script>

{@render children()}
