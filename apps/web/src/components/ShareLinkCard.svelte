<script lang="ts">
	import { formatModified } from '$lib/format';

	let {
		url,
		label,
		createdAt = '',
		ondismiss
	}: {
		url: string;
		label: string;
		createdAt?: string;
		ondismiss?: () => void;
	} = $props();

	type CopyState = 'idle' | 'done' | 'manual';

	let field: HTMLInputElement | null = $state(null);
	let status = $state<CopyState>('idle');

	function select(): void {
		field?.focus();
		field?.select();
	}

	async function copy(): Promise<void> {
		try {
			await navigator.clipboard.writeText(url);
			status = 'done';
		} catch {
			// 非安全上下文或权限被拒时剪贴板 API 不可用，退化为「链接已选中，手动复制」。
			status = 'manual';
			select();
		}
	}
</script>

<div class="share-card" role="status">
	<div class="head">
		<span class="badge" data-tone="ok">{label}已创建</span>
		{#if createdAt}
			<span class="time">{formatModified(createdAt)}</span>
		{/if}
		{#if ondismiss}
			<button class="btn" data-variant="ghost" type="button" onclick={() => ondismiss?.()}>
				关闭
			</button>
		{/if}
	</div>
	<div class="row">
		<input
			class="input mono url"
			type="text"
			readonly
			value={url}
			bind:this={field}
			onfocus={select}
			onclick={select}
			aria-label="分享链接"
		/>
		<button class="btn" type="button" onclick={() => void copy()}>复制链接</button>
	</div>
	{#if status === 'done'}
		<p class="hint ok-line">链接已复制到剪贴板。</p>
	{:else if status === 'manual'}
		<p class="hint warn-line">
			浏览器没有允许写入剪贴板，链接已选中：请按系统复制快捷键（⌘/Ctrl + C）或右键复制。
		</p>
	{/if}
</div>

<style>
	.share-card {
		display: flex;
		flex-direction: column;
		gap: 8px;
		margin: 12px 20px 0;
		padding: 10px 12px;
		border-radius: var(--radius-md);
		border: 1px solid color-mix(in oklch, var(--ok) 40%, transparent);
		background: color-mix(in oklch, var(--ok) 8%, transparent);
	}

	.head {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.head button {
		margin-left: auto;
	}

	.time {
		font-size: 10.5px;
		color: var(--muted-foreground);
		font-variant-numeric: tabular-nums;
	}

	.row {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	input.url {
		flex: 1;
		min-width: 0;
		font-size: 11.5px;
	}

	.hint {
		margin: 0;
		font-size: 11.5px;
		line-height: 1.6;
	}

	.ok-line {
		color: var(--ok);
	}

	.warn-line {
		color: var(--warn);
	}
</style>
