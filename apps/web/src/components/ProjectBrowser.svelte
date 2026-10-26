<script lang="ts">
	import { fsFailure, listConnectorDir, readConnectorText } from '$lib/api/workspace';
	import type { FsEntry, FsTextFile } from '$lib/api/types';
	import {
		describeFsFailure,
		fsBreadcrumb,
		parentFsPath,
		trimFsPath,
		type FsFailureView
	} from '$lib/fs';
	import { formatBytes, formatModified } from '$lib/format';
	import { workspace } from '$lib/workspace.svelte';

	let {
		token,
		connectorId,
		connectorName,
		projectName,
		workspacePath,
		onclose
	}: {
		token: string;
		connectorId: string;
		connectorName: string;
		projectName: string;
		workspacePath: string;
		onclose: () => void;
	} = $props();

	/** 设备上报的规范路径：workspacePath 可能是符号链接路径，设备会把它解析成真实路径。 */
	let reportedRoot = $state('');
	const configuredRoot = $derived(trimFsPath(workspacePath));
	const root = $derived(reportedRoot !== '' ? reportedRoot : configuredRoot);
	const connectorOnline = $derived(
		(workspace.connectorById(connectorId)?.status ?? 'offline') === 'online'
	);

	let path = $state('');
	let entries = $state<FsEntry[]>([]);
	let loading = $state(true);
	let listError = $state<FsFailureView | null>(null);

	let preview = $state<FsTextFile | null>(null);
	let previewPath = $state('');
	let previewLoading = $state(false);
	let previewError = $state<FsFailureView | null>(null);

	const crumbs = $derived(fsBreadcrumb(root, path));
	const upTarget = $derived(parentFsPath(path, root));

	let listTicket = 0;
	let readTicket = 0;

	let activeKey = '';

	$effect(() => {
		// key 只用配置路径：设备回报的规范路径会改 root，若把它也放进 key 就会自触发重取。
		const key = `${connectorId}::${configuredRoot}`;
		if (activeKey === key) return;
		activeKey = key;
		reportedRoot = '';
		void loadDir(configuredRoot);
	});

	async function loadDir(target: string): Promise<void> {
		const ticket = (listTicket += 1);
		readTicket += 1;
		loading = true;
		listError = null;
		entries = [];
		preview = null;
		previewPath = '';
		previewError = null;
		previewLoading = false;
		path = target;
		if (root === '') {
			loading = false;
			listError = describeFsFailure(null, connectorOnline);
			return;
		}
		try {
			const result = await listConnectorDir(token, connectorId, target);
			if (ticket !== listTicket) return;
			// 第一次回答就定下浏览根：之后的「上级」与面包屑都按设备自己的真实路径算，
			// 否则 /tmp 与 /private/tmp 这种符号链接会让根判定永远匹配不上。
			if (reportedRoot === '' && result.path !== '') reportedRoot = result.path;
			path = result.path;
			entries = result.entries;
			loading = false;
		} catch (error) {
			if (ticket !== listTicket) return;
			listError = describeFsFailure(fsFailure(error), connectorOnline);
			loading = false;
		}
	}

	async function openFile(entry: FsEntry): Promise<void> {
		const ticket = (readTicket += 1);
		previewPath = entry.path;
		previewLoading = true;
		previewError = null;
		preview = null;
		try {
			const file = await readConnectorText(token, connectorId, entry.path);
			if (ticket !== readTicket) return;
			preview = file;
			previewLoading = false;
		} catch (error) {
			if (ticket !== readTicket) return;
			previewError = describeFsFailure(fsFailure(error), connectorOnline);
			previewLoading = false;
		}
	}

	function selectEntry(entry: FsEntry): void {
		if (entry.kind === 'dir') {
			void loadDir(entry.path);
			return;
		}
		void openFile(entry);
	}
</script>

<div class="overlay" role="presentation">
	<div class="dialog" role="dialog" aria-modal="true" aria-label="浏览项目目录">
		<header class="dialog-head">
			<div class="head-text">
				<p class="dialog-title">浏览目录 · {projectName}</p>
				<p class="dialog-sub mono" title={root}>{root}</p>
			</div>
			<span class="badge" data-tone={connectorOnline ? 'ok' : 'err'}>
				<span class="dot"></span>
				{connectorName} · {connectorOnline ? '在线' : '离线'}
			</span>
			<button class="btn" data-variant="ghost" type="button" onclick={onclose}>关闭</button>
		</header>

		<div class="toolbar">
			<button
				class="btn"
				data-variant="ghost"
				type="button"
				data-testid="browser-up"
				disabled={loading || upTarget === null}
				onclick={() => upTarget !== null && void loadDir(upTarget)}
			>
				↑ 上级
			</button>
			<button
				class="btn"
				data-variant="ghost"
				type="button"
				disabled={loading || root === '' || path === root}
				onclick={() => void loadDir(root)}
			>
				回到 workspace 根
			</button>
			{#if reportedRoot !== '' && reportedRoot !== configuredRoot}
				<span class="hint resolved" title={reportedRoot}>
					设备把 {configuredRoot} 解析成 {reportedRoot}
				</span>
			{/if}
			<nav class="crumbs" aria-label="路径">
				{#each crumbs as crumb, index (crumb.path)}
					{#if index > 0}
						<span class="sep" aria-hidden="true">/</span>
					{/if}
					<button
						class="crumb mono"
						type="button"
						disabled={index === crumbs.length - 1}
						title={crumb.path}
						onclick={() => void loadDir(crumb.path)}
					>
						{crumb.name}
					</button>
				{/each}
			</nav>
		</div>

		<div class="body">
			<section class="list" aria-busy={loading} aria-label="目录内容">
				{#if loading}
					<p class="hint pad" data-testid="browser-loading">正在读取目录…</p>
				{:else if listError}
					<div class="fs-error pad" role="alert" data-testid="browser-error">
						<p class="err-title">{listError.title}</p>
						<p class="err-hint">{listError.hint}</p>
						{#if listError.serverMessage}
							<p class="server-msg mono">服务端：{listError.serverMessage}</p>
						{/if}
						{#if root !== '' && !listError.offline}
							<button class="btn" type="button" onclick={() => void loadDir(root)}>
								回到 workspace 根
							</button>
						{/if}
					</div>
				{:else if entries.length === 0}
					<p class="hint pad" data-testid="browser-empty">这个目录是空的。</p>
				{:else}
					<ul class="rows" data-testid="browser-entries">
						{#each entries as entry (entry.path)}
							<li>
								<button
									class="row"
									type="button"
									data-kind={entry.kind}
									title={entry.path}
									onclick={() => selectEntry(entry)}
								>
									<span class="icon" aria-hidden="true">{entry.kind === 'dir' ? '▸' : '·'}</span>
									<span class="name mono">{entry.name}</span>
									<span class="size">{entry.kind === 'dir' ? '目录' : formatBytes(entry.sizeBytes)}</span>
									<span class="time">{formatModified(entry.modifiedAt)}</span>
								</button>
							</li>
						{/each}
					</ul>
				{/if}
			</section>

			<section class="preview" aria-label="文件预览">
				{#if previewLoading}
					<p class="hint pad">正在读取文件…</p>
				{:else if previewError}
					<div class="fs-error pad" role="alert" data-testid="preview-error">
						<p class="err-title">{previewError.title}</p>
						<p class="err-hint">{previewError.hint}</p>
						{#if previewError.serverMessage}
							<p class="server-msg mono">服务端：{previewError.serverMessage}</p>
						{/if}
					</div>
				{:else if preview}
					<header class="preview-head">
						<span class="preview-path mono" title={preview.path}>{preview.path}</span>
						<span class="preview-meta">
							{#if preview.truncated}
								<span class="badge" data-tone="warn">内容已截断</span>
							{/if}
							<span class="time">{formatBytes(preview.sizeBytes)}</span>
						</span>
					</header>
					<pre class="preview-body mono">{preview.content}</pre>
				{:else if previewPath}
					<p class="hint pad">未取得内容。</p>
				{:else}
					<p class="hint pad">点一个文件查看文本内容（设备只允许 1 MiB 以内的文本）。</p>
				{/if}
			</section>
		</div>
	</div>
</div>

<svelte:window
	onkeydown={(event) => {
		if (event.key === 'Escape') onclose();
	}}
/>

<style>
	.overlay {
		position: fixed;
		inset: 0;
		z-index: 40;
		display: grid;
		place-items: center;
		padding: 24px;
		background: oklch(0 0 0 / 62%);
	}

	.dialog {
		display: flex;
		flex-direction: column;
		width: min(1080px, 100%);
		height: min(720px, 100%);
		border-radius: var(--radius-lg);
		border: 1px solid var(--border);
		background: var(--card);
		box-shadow: 0 24px 60px oklch(0 0 0 / 55%);
		overflow: hidden;
	}

	.dialog-head {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 16px;
		border-bottom: 1px solid var(--border);
	}

	.head-text {
		flex: 1;
		min-width: 0;
	}

	.dialog-title {
		font-size: 13.5px;
		font-weight: 600;
	}

	.dialog-sub {
		margin-top: 2px;
		font-size: 10.5px;
		color: var(--muted-foreground);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.toolbar {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-wrap: wrap;
		padding: 8px 16px;
		border-bottom: 1px solid var(--border);
	}

	.resolved {
		font-size: 10.5px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		max-width: 100%;
	}

	.crumbs {
		display: flex;
		align-items: center;
		gap: 2px;
		flex-wrap: wrap;
		min-width: 0;
	}

	.crumb {
		font-size: 11px;
		color: var(--info);
		padding: 1px 4px;
		border-radius: var(--radius-sm);
	}

	.crumb:disabled {
		color: var(--foreground);
		cursor: default;
	}

	.crumb:hover:not(:disabled) {
		background: oklch(1 0 0 / 8%);
	}

	.sep {
		font-size: 10px;
		color: var(--muted-foreground);
	}

	.body {
		flex: 1;
		min-height: 0;
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
	}

	.list {
		display: flex;
		flex-direction: column;
		min-height: 0;
		overflow-y: auto;
		border-right: 1px solid var(--border);
		padding: 4px 0;
	}

	.rows {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
	}

	.row {
		display: grid;
		grid-template-columns: 14px minmax(0, 1fr) auto auto;
		align-items: center;
		gap: 10px;
		width: 100%;
		padding: 6px 16px;
		text-align: left;
		border-radius: var(--radius-sm);
	}

	.row:hover {
		background: oklch(1 0 0 / 6%);
	}

	.row .icon {
		color: var(--muted-foreground);
		font-size: 11px;
	}

	.row[data-kind='dir'] .name {
		color: var(--info);
	}

	.name {
		font-size: 11.5px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.size,
	.time {
		font-size: 10px;
		color: var(--muted-foreground);
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}

	.preview {
		display: flex;
		flex-direction: column;
		min-height: 0;
	}

	.preview-head {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 8px 16px;
		border-bottom: 1px solid var(--border);
	}

	.preview-path {
		flex: 1;
		font-size: 10.5px;
		color: var(--muted-foreground);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.preview-meta {
		display: flex;
		align-items: center;
		gap: 6px;
		flex: none;
	}

	.preview-body {
		margin: 0;
		padding: 10px 16px 16px;
		flex: 1;
		min-height: 0;
		overflow: auto;
		font-size: 11.5px;
		line-height: 1.6;
		white-space: pre;
		tab-size: 2;
	}

	.pad {
		padding: 10px 16px;
	}

	.hint {
		font-size: 11.5px;
		color: var(--muted-foreground);
		line-height: 1.6;
	}

	.fs-error {
		display: flex;
		flex-direction: column;
		gap: 6px;
		margin: 8px 16px;
		padding: 10px 12px;
		border-radius: var(--radius-md);
		border: 1px solid color-mix(in oklch, var(--err) 42%, transparent);
		background: color-mix(in oklch, var(--err) 10%, transparent);
	}

	.err-title {
		font-size: 12px;
		font-weight: 600;
		color: oklch(0.86 0.09 22);
	}

	.err-hint {
		font-size: 11px;
		color: var(--muted-foreground);
		line-height: 1.6;
	}

	.server-msg {
		font-size: 10.5px;
		color: oklch(0.86 0.06 22);
		word-break: break-word;
	}
</style>