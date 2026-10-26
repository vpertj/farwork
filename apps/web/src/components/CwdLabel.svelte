<script lang="ts">
	import { truncateMiddle } from '$lib/format';

	let {
		cwd,
		maxLength = 64,
		label = ''
	}: { cwd: string | null; maxLength?: number; label?: string } = $props();

	const path = $derived((cwd ?? '').trim());
	const shown = $derived(truncateMiddle(path, maxLength));

	let copied = $state(false);
	let copyFailed = $state(false);
	let timer: ReturnType<typeof setTimeout> | null = null;

	async function copy(): Promise<void> {
		if (path.length === 0) return;
		try {
			await navigator.clipboard.writeText(path);
			copied = true;
			copyFailed = false;
		} catch {
			copied = false;
			copyFailed = true;
		}
		if (timer) clearTimeout(timer);
		timer = setTimeout(() => {
			copied = false;
			copyFailed = false;
		}, 1_800);
	}

	$effect(() => () => {
		if (timer) clearTimeout(timer);
	});
</script>

{#if label}
	<span class="cwd-label">{label}</span>
{/if}
{#if path.length === 0}
	<span class="cwd-unset" title="这条会话没有上报工作目录">未上报工作目录</span>
{:else}
	<span class="cwd">
		<span class="cwd-path mono" title={path}>{shown}</span>
		<button
			class="btn"
			data-variant="ghost"
			type="button"
			title="复制完整工作目录"
			onclick={() => void copy()}
		>
			{copied ? '已复制' : '复制'}
		</button>
		{#if copyFailed}
			<span class="cwd-copy-hint">剪贴板不可用，请手动选中路径复制</span>
		{/if}
	</span>
{/if}

<style>
	.cwd {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		min-width: 0;
		max-width: 100%;
	}

	.cwd-path {
		font-size: 11px;
		color: var(--muted-foreground);
		white-space: nowrap;
		overflow: hidden;
	}

	.cwd-label {
		font-size: 10.5px;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--muted-foreground);
	}

	.cwd-unset {
		font-size: 11px;
		color: var(--warn);
	}

	.cwd-copy-hint {
		font-size: 10.5px;
		color: var(--warn);
	}
</style>
