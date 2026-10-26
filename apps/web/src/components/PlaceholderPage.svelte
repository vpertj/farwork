<script lang="ts">
	let {
		title,
		description,
		backLabel = '返回任务台'
	}: { title: string; description: string; backLabel?: string } = $props();
</script>

<section class="page">
	<header>
		<span class="badge">占位页面</span>
		<h1>{title}</h1>
		<p>{description}</p>
	</header>
	<div class="panel">
		<p>该页面已注册路由，但不在本轮纵向切片范围内，因此只提供不白屏的占位内容。</p>
		<a class="btn" href="#/">{backLabel}</a>
	</div>
</section>

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 20px;
		padding: 32px;
		max-width: 760px;
	}

	header {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	h1 {
		font-size: 22px;
		letter-spacing: -0.01em;
	}

	header p {
		font-size: 13px;
		color: var(--muted-foreground);
		font-weight: 400;
		line-height: 1.65;
	}

	.panel {
		display: flex;
		flex-direction: column;
		gap: 16px;
		align-items: flex-start;
		padding: 20px;
		border-radius: var(--radius-lg);
		border: 1px dashed var(--border);
		background: oklch(1 0 0 / 3%);
		font-size: 13px;
		color: var(--muted-foreground);
		font-weight: 400;
		line-height: 1.65;
	}
</style>
