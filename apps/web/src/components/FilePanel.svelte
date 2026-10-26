<script lang="ts">
	import { listSessionDir, readSessionText, fsFailure } from '$lib/api/workspace';
	import type { FsEntry, FsTextFile } from '$lib/api/types';
	import { describeFsFailure, fsBreadcrumb, trimFsPath, type FsFailureView } from '$lib/fs';
	import { formatBytes, formatModified } from '$lib/format';

	let {
		token,
		sessionId,
		workspacePath,
		connectorOnline
	}: {
		token: string;
		sessionId: string;
		workspacePath: string;
		connectorOnline: boolean;
	} = $props();

	let path = $state('');
	let entries = $state<FsEntry[]>([]);
	let listing = $state(true);
	let listError = $state<FsFailureView | null>(null);

	let preview = $state<FsTextFile | null>(null);
	let previewPath = $state('');
	let previewLoading = $state(false);
	let previewError = $state<FsFailureView | null>(null);

	const root = $derived(trimFsPath(workspacePath));
	const crumbs = $derived(fsBreadcrumb(root, path));

	let listTicket = 0;
	let readTicket = 0;
	let activeKey = '';

	$effect(() => {
		const key = `${token}::${sessionId}::${root}::${connectorOnline}`;
		if (activeKey === key) return;
		activeKey = key;
		listTicket += 1;
		readTicket += 1;
		entries = [];
		listError = null;
		preview = null;
		previewPath = '';
		previewError = null;
		previewLoading = false;
		path = root;
		if (root === '') {
			listing = false;
			return;
		}
		void loadDir(root);
	});

	async function loadDir(target: string): Promise<void> {
		const ticket = (listTicket += 1);
		readTicket += 1;
		listing = true;
		listError = null;
		entries = [];
		preview = null;
		previewPath = '';
		previewError = null;
		previewLoading = false;
		path = target;
		if (!connectorOnline) {
			listing = false;
			listError = describeFsFailure(null, false);
			return;
		}
		try {
			const result = await listSessionDir(token, sessionId, target);
			if (ticket !== listTicket) return;
			path = result.path;
			entries = result.entries;
			listing = false;
		} catch (error) {
			if (ticket !== listTicket) return;
			listError = describeFsFailure(fsFailure(error), connectorOnline);
			listing = false;
		}
	}

	async function openFile(entry: FsEntry): Promise<void> {
		const ticket = (readTicket += 1);
		previewPath = entry.path;
		previewLoading = true;
		previewError = null;
		preview = null;
		try {
			const file = await readSessionText(token, sessionId, entry.path);
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

	function reload(): void {
		if (root === '') return;
		void loadDir(path || root);
	}
</script>

<aside class="files" aria-label="文件浏览">
	<header class="panel-head">
		<span class="panel-title">文件</span>
		<span class="badge" data-tone={connectorOnline ? 'ok' : 'err'}>
			<span class="dot"></span>
			{connectorOnline ? '设备在线' : '设备离线'}
		</span>
		<button
			class="btn"
			data-variant="ghost"
			type="button"
			disabled={listing || root === ''}
			onclick={reload}
		>
			{listing ? '读取中…' : '刷新'}
		</button>
	</header>

	<p class="root mono" title={root || '未设置工作目录'}>{root || '未设置工作目录'}</p>

	<nav class="crumbs" aria-label="路径">
		{#if root === ''}
			<span class="hint">尚未得知这个会话的工作目录（workspacePath）。</span>
		{:else}
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
		{/if}
	</nav>

	<div class="list" aria-busy={listing}>
		{#if listing}
			<p class="hint pad">正在读取目录…</p>
		{:else if listError}
			<div class="fs-error pad" role="alert">
				<p class="err-title">{listError.title}</p>
				<p class="err-hint">{listError.hint}</p>
				{#if listError.serverMessage}
					<p class="server-msg mono">服务端：{listError.serverMessage}</p>
				{/if}
				{#if root !== '' && !listError.offline}
					<button class="btn" type="button" onclick={() => void loadDir(root)}>
						回到工作目录根
					</button>
				{/if}
			</div>
		{:else if root === ''}
			<p class="hint pad">没有工作目录可以浏览。</p>
		{:else if entries.length === 0}
			<p class="hint pad">这个目录是空的。</p>
		{:else}
			{#each entries as entry (entry.path)}
				<button
					class="row"
					type="button"
					data-kind={entry.kind}
					title={entry.path}
					onclick={() => selectEntry(entry)}
				>
					<span class="icon" aria-hidden="true">{entry.kind === 'dir' ? '▸' : '·'}</span>
					<span class="name mono">{entry.name}</span>
					<span class="size">{entry.kind === 'dir' ? '—' : formatBytes(entry.sizeBytes)}</span>
					<span class="time">{formatModified(entry.modifiedAt)}</span>
				</button>
			{/each}
		{/if}
	</div>

	<section class="preview" aria-label="文件预览">
		{#if previewLoading}
			<p class="hint pad">正在读取文件…</p>
		{:else if previewError}
			<div class="fs-error pad" role="alert">
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
			{#if preview.truncated}
				<p class="truncated-note">
					文件共 {formatBytes(preview.sizeBytes)}，这里只显示前一段内容；完整内容请在设备上查看。
				</p>
			{/if}
			<pre class="preview-body mono">{preview.content}</pre>
		{:else if previewPath}
			<p class="hint pad">未取得内容。</p>
		{:else}
			<p class="hint pad">选一个文件查看文本内容。</p>
		{/if}
	</section>
</aside>

<style>
	.files {
		display: flex;
		flex-direction: column;
		width: 380px;
		flex: none;
		min-height: 0;
		height: 100vh;
		border-left: 1px solid var(--border);
		background: oklch(0.17 0 0);
	}

	@media (max-width: 960px) {
		.files {
			width: min(320px, 45vw);
		}
	}

	.panel-head {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 12px 14px;
		border-bottom: 1px solid var(--border);
	}

	.panel-title {
		font-size: 13px;
		font-weight: 600;
		flex: 1;
	}

	.root {
		padding: 8px 14px 0;
		font-size: 10.5px;
		color: var(--muted-foreground);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.crumbs {
		display: flex;
		align-items: center;
		gap: 2px;
		flex-wrap: wrap;
		padding: 6px 14px 8px;
		border-bottom: 1px solid var(--border);
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

	.list {
		flex: 1;
		min-height: 120px;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		padding: 4px 0;
	}

	.row {
		display: grid;
		grid-template-columns: 14px minmax(0, 1fr) auto auto;
		align-items: center;
		gap: 8px;
		padding: 5px 12px;
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

	.pad {
		padding: 10px 14px;
	}

	.hint {
		font-size: 11px;
		color: var(--muted-foreground);
		line-height: 1.6;
	}

	.fs-error {
		display: flex;
		flex-direction: column;
		gap: 6px;
		margin: 8px 12px;
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

	.preview {
		display: flex;
		flex-direction: column;
		min-height: 0;
		border-top: 1px solid var(--border);
		flex: 1;
	}

	.preview-head {
		display: flex;
		align-items: center;
		gap: 8px;
		padding: 8px 14px;
		border-bottom: 1px solid var(--border);
	}

	.preview-path {
		font-size: 10.5px;
		color: var(--muted-foreground);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		flex: 1;
	}

	.preview-meta {
		display: flex;
		align-items: center;
		gap: 6px;
		flex: none;
	}

	.truncated-note {
		padding: 6px 14px;
		font-size: 10.5px;
		color: var(--warn);
		background: color-mix(in oklch, var(--warn) 10%, transparent);
	}

	.preview-body {
		margin: 0;
		padding: 10px 14px 16px;
		flex: 1;
		min-height: 0;
		overflow: auto;
		font-size: 11.5px;
		line-height: 1.6;
		white-space: pre;
		tab-size: 2;
	}
</style>
