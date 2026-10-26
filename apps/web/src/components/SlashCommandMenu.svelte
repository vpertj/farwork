<script lang="ts">
	import type { RuntimeCommand } from '$lib/api/types';

	let {
		items,
		activeIndex,
		onpick
	}: {
		items: RuntimeCommand[];
		activeIndex: number;
		onpick: (name: string) => void;
	} = $props();
</script>

<ul class="menu" role="listbox" aria-label="斜杠命令">
	{#each items as item, index (item.name)}
		<li
			role="option"
			tabindex="-1"
			aria-selected={index === activeIndex}
			class:active={index === activeIndex}
			onmousedown={(event) => {
				event.preventDefault();
				onpick(item.name);
			}}
		>
			<span class="name mono">/{item.name}</span>
			{#if item.description.length > 0}
				<span class="desc">{item.description}</span>
			{/if}
		</li>
	{/each}
</ul>

<style>
	.menu {
		display: flex;
		flex-direction: column;
		margin: 0;
		padding: 4px;
		list-style: none;
		border-radius: var(--radius-md);
		border: 1px solid var(--border);
		background: oklch(0.2 0 0);
		box-shadow: 0 10px 24px oklch(0 0 0 / 45%);
	}

	li {
		display: flex;
		align-items: baseline;
		gap: 8px;
		padding: 5px 8px;
		border-radius: var(--radius-sm);
		cursor: pointer;
	}

	li.active {
		background: oklch(1 0 0 / 10%);
	}

	.name {
		font-size: 12px;
		color: var(--info);
		white-space: nowrap;
	}

	.desc {
		font-size: 11px;
		color: var(--muted-foreground);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
</style>
