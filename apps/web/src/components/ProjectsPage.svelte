<script lang="ts">
	import { onMount } from 'svelte';
	import { workspace } from '$lib/workspace.svelte';
	import {
		createProject,
		deleteProject,
		listProjectSessions,
		listProjects,
		updateProject
	} from '$lib/api/workspace';
	import { describeError, formatRelative } from '$lib/format';
	import { sessionStatusLabel, sessionStatusTone } from '$lib/timeline';
	import type { Project, Session } from '$lib/api/types';
	import CwdLabel from './CwdLabel.svelte';
	import ProjectBrowser from './ProjectBrowser.svelte';

	let { token }: { token: string } = $props();

	let projects = $state<Project[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	let formConnectorId = $state('');
	let formPath = $state('');
	let formName = $state('');
	let nameEdited = $state(false);
	let creating = $state(false);
	let formError = $state<string | null>(null);

	let renamingId = $state<string | null>(null);
	let renameValue = $state('');
	let renamingBusy = $state(false);
	let renameError = $state<string | null>(null);

	let pinBusyId = $state<string | null>(null);
	let actionError = $state<string | null>(null);
	let notice = $state<string | null>(null);
	let copiedId = $state<string | null>(null);
	let copyTimer: ReturnType<typeof setTimeout> | null = null;

	let confirmingId = $state<string | null>(null);
	let deletingId = $state<string | null>(null);
	let deleteError = $state<string | null>(null);

	/** 正在浏览目录的项目：null = 弹窗关闭。 */
	let browsing = $state<Project | null>(null);

	let expandedId = $state<string | null>(null);
	let expandedSessions = $state<Session[]>([]);
	let expandedLoading = $state(false);
	let expandedError = $state<string | null>(null);

	const connectors = $derived(workspace.connectors);
	const sortedProjects = $derived(sortProjects(projects));
	const onlineCount = $derived(connectors.filter((row) => row.status === 'online').length);

	function activityRank(value: string | null): number {
		if (!value) return -1;
		const timestamp = Date.parse(value);
		return Number.isNaN(timestamp) ? -1 : timestamp;
	}

	function sortProjects(rows: readonly Project[]): Project[] {
		return [...rows].sort((left, right) => {
			if (left.pinned !== right.pinned) return left.pinned ? -1 : 1;
			return activityRank(right.lastActivityAt) - activityRank(left.lastActivityAt);
		});
	}

	function deviceName(connectorId: string): string {
		return workspace.connectorById(connectorId)?.name ?? '未知设备';
	}

	function deviceStatus(connectorId: string): string {
		return workspace.connectorById(connectorId)?.status ?? 'offline';
	}

	function basenameOf(path: string): string {
		const trimmed = path.trim().replace(/\/+$/, '');
		const index = trimmed.lastIndexOf('/');
		return index >= 0 ? trimmed.slice(index + 1) : trimmed;
	}

	function replaceProject(next: Project): void {
		projects = projects.map((row) => (row.id === next.id ? next : row));
	}

	async function load(): Promise<void> {
		loading = true;
		loadError = null;
		try {
			projects = await listProjects(token);
		} catch (error) {
			projects = [];
			loadError = describeError(error);
		} finally {
			loading = false;
		}
	}

	function updatePath(event: Event): void {
		const target = event.currentTarget;
		if (!(target instanceof HTMLInputElement)) return;
		formPath = target.value;
		if (!nameEdited) formName = basenameOf(target.value);
	}

	function updateName(event: Event): void {
		const target = event.currentTarget;
		if (!(target instanceof HTMLInputElement)) return;
		formName = target.value;
		nameEdited = target.value.trim().length > 0;
	}

	function selectConnector(event: Event): void {
		const target = event.currentTarget;
		if (!(target instanceof HTMLSelectElement)) return;
		formConnectorId = target.value;
	}

	function resetForm(): void {
		formConnectorId = '';
		formPath = '';
		formName = '';
		nameEdited = false;
	}

	async function submit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		formError = null;
		notice = null;
		actionError = null;
		const path = formPath.trim();
		const name = formName.trim();
		if (formConnectorId.length === 0) {
			formError = '请选择设备：项目必须绑定到一台设备上。';
			return;
		}
		if (path.length === 0) {
			formError = '请填写 workspace 绝对路径。';
			return;
		}
		if (!path.startsWith('/')) {
			formError = 'workspace 路径必须是绝对路径（以 / 开头）。';
			return;
		}
		if (name.length === 0) {
			formError = '请填写项目名称。';
			return;
		}
		creating = true;
		try {
			const created = await createProject(token, {
				connectorId: formConnectorId,
				workspacePath: path,
				name
			});
			projects = [...projects, created];
			notice = `已创建项目「${created.name}」。`;
			resetForm();
			void workspace.load(token);
		} catch (error) {
			formError = describeError(error);
		} finally {
			creating = false;
		}
	}

	function startRename(project: Project): void {
		renamingId = project.id;
		renameValue = project.name;
		renameError = null;
		actionError = null;
	}

	function cancelRename(): void {
		renamingId = null;
		renameError = null;
	}

	async function submitRename(project: Project): Promise<void> {
		const next = renameValue.trim();
		renameError = null;
		if (next.length === 0) {
			renameError = '项目名称不能为空。';
			return;
		}
		if (next === project.name) {
			cancelRename();
			return;
		}
		renamingBusy = true;
		try {
			const updated = await updateProject(token, project.id, { name: next });
			replaceProject(updated);
			notice = `已重命名为「${updated.name}」。`;
			renamingId = null;
			void workspace.load(token);
		} catch (error) {
			renameError = describeError(error);
		} finally {
			renamingBusy = false;
		}
	}

	async function togglePin(project: Project): Promise<void> {
		pinBusyId = project.id;
		actionError = null;
		notice = null;
		try {
			const updated = await updateProject(token, project.id, { pinned: !project.pinned });
			replaceProject(updated);
			notice = updated.pinned ? `已置顶「${updated.name}」。` : `已取消置顶「${updated.name}」。`;
			void workspace.load(token);
		} catch (error) {
			actionError = describeError(error);
		} finally {
			pinBusyId = null;
		}
	}

	function askDelete(project: Project): void {
		confirmingId = project.id;
		deleteError = null;
		actionError = null;
	}

	function cancelDelete(): void {
		confirmingId = null;
		deleteError = null;
	}

	async function confirmDelete(project: Project): Promise<void> {
		deletingId = project.id;
		deleteError = null;
		try {
			const result = await deleteProject(token, project.id);
			projects = projects.filter((row) => row.id !== result.projectId);
			if (expandedId === result.projectId) {
				expandedId = null;
				expandedSessions = [];
			}
			if (renamingId === result.projectId) renamingId = null;
			confirmingId = null;
			notice = `已解绑 ${result.detachedSessions} 个会话。`;
			void workspace.load(token);
		} catch (error) {
			deleteError = describeError(error);
		} finally {
			deletingId = null;
		}
	}

	async function toggleSessions(project: Project): Promise<void> {
		if (expandedId === project.id) {
			expandedId = null;
			expandedSessions = [];
			expandedError = null;
			return;
		}
		expandedId = project.id;
		expandedSessions = [];
		expandedError = null;
		expandedLoading = true;
		try {
			expandedSessions = await listProjectSessions(token, project.id);
		} catch (error) {
			expandedError = describeError(error);
		} finally {
			expandedLoading = false;
		}
	}

	async function copyPath(project: Project): Promise<void> {
		try {
			await navigator.clipboard.writeText(project.workspacePath);
			copiedId = project.id;
			if (copyTimer) clearTimeout(copyTimer);
			copyTimer = setTimeout(() => {
				copiedId = null;
			}, 1600);
		} catch {
			actionError = '复制失败：浏览器拒绝了剪贴板写入，请手动选择路径文本。';
		}
	}

	onMount(() => {
		void load();
		return () => {
			if (copyTimer) clearTimeout(copyTimer);
		};
	});
</script>

<section class="projects">
	<header>
		<div>
			<h1>项目</h1>
			<p>
				项目 = 设备上的一个工作目录，会话挂在它下面。创建后可用
				<code class="mono">PATCH</code> 重命名与置顶，删除只会解绑会话、不删会话。
			</p>
		</div>
		<button class="btn" data-variant="ghost" type="button" onclick={load} disabled={loading}>
			{loading ? '刷新中' : '刷新'}
		</button>
	</header>

	{#if notice}
		<p class="alert notice" role="status">{notice}</p>
	{/if}
	{#if actionError}
		<p class="alert" role="alert">{actionError}</p>
	{/if}

	<form class="create" onsubmit={submit}>
		<h2>新建项目</h2>
		<label class="field">
			<span>设备</span>
			{#if connectors.length === 0}
				<select class="input" disabled>
					<option value="">还没有任何设备</option>
				</select>
			{:else}
				<select class="input" value={formConnectorId} onchange={selectConnector}>
					<option value="">请选择在线设备</option>
					{#each connectors as connector (connector.id)}
						<option value={connector.id} disabled={connector.status !== 'online'}>
							{connector.name} · {connector.status === 'online' ? '在线' : '离线（不可绑定）'}
						</option>
					{/each}
				</select>
			{/if}
		</label>

		{#if connectors.length > 0 && onlineCount === 0}
			<p class="note-line">
				所有设备当前都离线，无法新建项目：工作目录绑定需要设备在线才能确认路径。
			</p>
		{:else if connectors.some((row) => row.status !== 'online')}
			<ul class="offline">
				{#each connectors.filter((row) => row.status !== 'online') as connector (connector.id)}
					<li>
						<strong>{connector.name}</strong> 离线（
						{connector.lastSeenAt ? `最后在线 ${formatRelative(connector.lastSeenAt)}` : '从未连上 hub'}
						），已置灰：设备不在线时不能新建挂在该设备上的工作目录。
					</li>
				{/each}
			</ul>
		{/if}

		<label class="field">
			<span>workspace 绝对路径</span>
			<input
				class="input mono"
				type="text"
				placeholder="/srv/work/app"
				value={formPath}
				oninput={updatePath}
			/>
		</label>

		<label class="field">
			<span>项目名称</span>
			<input
				class="input"
				type="text"
				placeholder="默认取路径最后一段"
				value={formName}
				oninput={updateName}
			/>
		</label>

		{#if formError}
			<p class="alert" role="alert">{formError}</p>
		{/if}

		<button class="btn" data-variant="primary" type="submit" disabled={creating}>
			{creating ? '正在创建…' : '创建项目'}
		</button>
	</form>

	<div class="list-head">
		<h2>全部项目</h2>
		<span class="count">{projects.length}</span>
	</div>

	{#if loading && projects.length === 0}
		<div class="empty"><p>正在加载项目…</p></div>
	{:else if loadError}
		<div class="empty">
			<p>加载项目失败：{loadError}</p>
			<button class="btn" data-variant="ghost" type="button" onclick={load}>重试</button>
		</div>
	{:else if projects.length === 0}
		<div class="empty">
			<p>还没有任何项目。</p>
			<p>项目 = 设备上的一个工作目录，会话挂在它下面。用上面的表单创建第一个。</p>
		</div>
	{:else}
		<ul class="cards">
			{#each sortedProjects as project (project.id)}
				<li class="card">
					<div class="head">
						<button
							class="toggle"
							type="button"
							aria-expanded={expandedId === project.id}
							onclick={() => toggleSessions(project)}
						>
							<span class="caret" aria-hidden="true">{expandedId === project.id ? '▾' : '▸'}</span>
							<span class="name">{project.name}</span>
							{#if project.pinned}
								<span class="badge" data-tone="info">置顶</span>
							{/if}
							<span class="sr-only">展开该项目下的会话</span>
						</button>
						<div class="actions">
							<button
								class="btn"
								data-variant="ghost"
								type="button"
								disabled={pinBusyId === project.id}
								onclick={() => togglePin(project)}
							>
								{pinBusyId === project.id ? '提交中…' : project.pinned ? '取消置顶' : '置顶'}
							</button>
							<button
								class="btn"
								data-variant="ghost"
								type="button"
								data-testid="browse-project-dir"
								onclick={() => (browsing = project)}
							>
								浏览目录
							</button>
							<button
								class="btn"
								data-variant="ghost"
								type="button"
								onclick={() => startRename(project)}
							>
								重命名
							</button>
							<button
								class="btn"
								data-variant="ghost"
								type="button"
								onclick={() => askDelete(project)}
							>
								删除
							</button>
						</div>
					</div>

					{#if renamingId === project.id}
						<form
							class="rename"
							onsubmit={(event) => {
								event.preventDefault();
								void submitRename(project);
							}}
						>
							<input class="input" type="text" bind:value={renameValue} aria-label="新的项目名称" />
							<button class="btn" data-variant="primary" type="submit" disabled={renamingBusy}>
								{renamingBusy ? '保存中…' : '保存'}
							</button>
							<button class="btn" data-variant="ghost" type="button" onclick={cancelRename}>
								取消
							</button>
						</form>
						{#if renameError}
							<p class="alert" role="alert">{renameError}</p>
						{/if}
					{/if}

					<dl class="meta">
						<div>
							<dt>设备</dt>
							<dd>
								<span class="dot" data-tone={deviceStatus(project.connectorId) === 'online' ? 'ok' : 'muted'}></span>
								{deviceName(project.connectorId)}
								<span class="device-state">
									{deviceStatus(project.connectorId) === 'online' ? '在线' : '离线'}
								</span>
							</dd>
						</div>
						<div>
							<dt>workspace</dt>
							<dd>
								<code class="mono path">{project.workspacePath}</code>
								<button
									class="btn"
									data-variant="ghost"
									type="button"
									title="复制路径"
									onclick={() => copyPath(project)}
								>
									{copiedId === project.id ? '已复制' : '复制'}
								</button>
							</dd>
						</div>
						<div>
							<dt>活跃会话</dt>
							<dd>{project.activeSessionCount}</dd>
						</div>
						<div>
							<dt>最后活动</dt>
							<dd>{formatRelative(project.lastActivityAt)}</dd>
						</div>
					</dl>

					{#if confirmingId === project.id}
						<div class="confirm" role="alertdialog" aria-label="确认删除项目">
							<p class="confirm-title">确认删除「{project.name}」？</p>
							<p class="confirm-body">
								项目下的会话不会被删除，只是不再归属于该项目（sessions.project_id 置空）。
							</p>
							{#if deleteError}
								<p class="alert" role="alert">{deleteError}</p>
							{/if}
							<div class="confirm-actions">
								<button
									class="btn danger"
									type="button"
									disabled={deletingId === project.id}
									onclick={() => confirmDelete(project)}
								>
									{deletingId === project.id ? '删除中…' : '确认删除'}
								</button>
								<button class="btn" data-variant="ghost" type="button" onclick={cancelDelete}>
									取消
								</button>
							</div>
						</div>
					{/if}

					{#if expandedId === project.id}
						<div class="sessions">
							{#if expandedLoading}
								<p class="muted">正在加载会话…</p>
							{:else if expandedError}
								<p class="alert" role="alert">加载会话失败：{expandedError}</p>
							{:else if expandedSessions.length === 0}
								<p class="muted">这个项目下还没有会话。</p>
							{:else}
								<ul>
									{#each expandedSessions as session (session.id)}
										<li>
											<a class="session" href="#/session/{encodeURIComponent(session.id)}">
												<span class="dot" data-tone={sessionStatusTone(session.status)}></span>
												<span class="session-title">{session.title ?? session.id}</span>
												<span class="session-status">{sessionStatusLabel(session.status)}</span>
												<span class="session-time">{formatRelative(session.lastActivityAt)}</span>
											</a>
											<span class="session-cwd">
												<CwdLabel cwd={session.cwd} maxLength={56} />
											</span>
										</li>
									{/each}
								</ul>
							{/if}
							<a class="new-session" href="#/new-session/{encodeURIComponent(project.id)}">
								在该项目下新建会话 →
							</a>
						</div>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</section>

{#if browsing}
	<ProjectBrowser
		{token}
		connectorId={browsing.connectorId}
		connectorName={deviceName(browsing.connectorId)}
		projectName={browsing.name}
		workspacePath={browsing.workspacePath}
		onclose={() => (browsing = null)}
	/>
{/if}

<style>
	.projects {
		display: flex;
		flex-direction: column;
		gap: 16px;
		padding: 28px 32px 48px;
		height: 100vh;
		overflow-y: auto;
		max-width: 900px;
	}

	header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 12px;
	}

	h1 {
		font-size: 20px;
		letter-spacing: -0.01em;
	}

	h2 {
		font-size: 13px;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--muted-foreground);
	}

	header p {
		margin-top: 6px;
		font-size: 12.5px;
		color: var(--muted-foreground);
		font-weight: 400;
		line-height: 1.6;
	}

	.create {
		display: flex;
		flex-direction: column;
		gap: 10px;
		padding: 16px;
		border: 1px solid var(--border);
		border-radius: var(--radius-md);
		background: var(--card);
	}

	select.input {
		height: 38px;
	}

	.note-line {
		font-size: 11.5px;
		color: var(--warn);
	}

	.offline {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 3px;
		font-size: 11px;
		color: var(--muted-foreground);
	}

	.notice {
		border-color: color-mix(in oklch, var(--ok) 45%, transparent);
		background: color-mix(in oklch, var(--ok) 12%, transparent);
	}

	.list-head {
		display: flex;
		align-items: baseline;
		gap: 8px;
	}

	.count {
		font-size: 11.5px;
		color: var(--muted-foreground);
		font-variant-numeric: tabular-nums;
	}

	.cards {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.card {
		display: flex;
		flex-direction: column;
		gap: 10px;
		padding: 14px 16px;
		border: 1px solid var(--border);
		border-radius: var(--radius-md);
		background: var(--card);
	}

	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 10px;
	}

	.toggle {
		display: flex;
		align-items: center;
		gap: 8px;
		min-width: 0;
		padding: 2px 4px 2px 0;
		border-radius: var(--radius-sm);
		font-size: 13.5px;
		font-weight: 600;
		color: var(--foreground);
		text-align: left;
	}

	.toggle:hover {
		background: oklch(1 0 0 / 5%);
	}

	.caret {
		color: var(--muted-foreground);
		font-size: 11px;
	}

	.name {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.actions {
		display: flex;
		gap: 4px;
		flex: none;
	}

	.meta {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(190px, 1fr));
		gap: 6px 18px;
		margin: 0;
		font-size: 12px;
	}

	.meta div {
		display: flex;
		flex-direction: column;
		gap: 2px;
		min-width: 0;
	}

	.meta dt {
		font-size: 10.5px;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--muted-foreground);
	}

	.meta dd {
		display: flex;
		align-items: center;
		gap: 6px;
		margin: 0;
		min-width: 0;
	}

	.path {
		font-size: 11.5px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.device-state {
		font-size: 10.5px;
		color: var(--muted-foreground);
	}

	.rename {
		display: flex;
		gap: 6px;
	}

	.rename .input {
		flex: 1;
	}

	.confirm {
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 12px 14px;
		border-radius: var(--radius-sm);
		border: 1px solid color-mix(in oklch, var(--err) 45%, transparent);
		background: color-mix(in oklch, var(--err) 10%, transparent);
		font-size: 12.5px;
		line-height: 1.6;
	}

	.confirm-title {
		font-weight: 600;
	}

	.confirm-body {
		color: var(--muted-foreground);
	}

	.confirm-actions {
		display: flex;
		gap: 6px;
	}

	.btn.danger {
		border-color: color-mix(in oklch, var(--err) 55%, transparent);
		background: color-mix(in oklch, var(--err) 22%, transparent);
		color: var(--foreground);
	}

	.sessions {
		display: flex;
		flex-direction: column;
		gap: 6px;
		padding: 10px 12px;
		border-radius: var(--radius-sm);
		background: oklch(1 0 0 / 3%);
	}

	.sessions ul {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
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

	.session-title {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.session-cwd {
		display: flex;
		align-items: center;
		padding: 0 8px 4px 22px;
	}

	.session-status,
	.session-time {
		font-size: 10.5px;
		font-variant-numeric: tabular-nums;
	}

	.new-session {
		font-size: 11.5px;
		color: var(--info);
	}

	.muted {
		font-size: 11.5px;
		color: var(--muted-foreground);
	}

	.dot[data-tone='ok'] {
		background: var(--ok);
	}

	.dot[data-tone='warn'] {
		background: var(--warn);
	}

	.dot[data-tone='err'] {
		background: var(--err);
	}

	.dot[data-tone='info'] {
		background: var(--info);
	}

	.dot[data-tone='muted'] {
		background: var(--muted-foreground);
	}
</style>