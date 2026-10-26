<script lang="ts">
	import {
		createAdminUser,
		fetchAdminSettings,
		listAdminUsers,
		updateAdminSettings,
		updateAdminUser,
		type AdminSettingsUpdate,
		type AdminUserUpdate
	} from '$lib/api/admin';
	import {
		ADMIN_ROLES,
		type AdminRole,
		type AdminSettings,
		type AdminUser
	} from '$lib/api/types';
	import { untrack } from 'svelte';
	import { describeError, formatModified } from '$lib/format';

	let {
		token,
		currentUserId,
		currentUserRole
	}: { token: string; currentUserId: string; currentUserRole: string } = $props();

	const PAGE_SIZE = 20;

	let users = $state<AdminUser[]>([]);
	let nextCursor = $state<string | null>(null);
	let cursors = $state<(string | null)[]>([null]);
	let pageIndex = $state(0);
	let listLoading = $state(false);
	let listError = $state<string | null>(null);
	let listFlash = $state<string | null>(null);
	let query = $state('');

	let rowBusy = $state<Record<string, boolean>>({});
	let rowError = $state<{ id: string; message: string } | null>(null);
	let nameDrafts = $state<Record<string, string>>({});

	let newEmail = $state('');
	let newPassword = $state('');
	let newRole = $state<AdminRole>('member');
	let createBusy = $state(false);
	let createError = $state<string | null>(null);
	let createFlash = $state<string | null>(null);

	let settings = $state<AdminSettings | null>(null);
	let settingsLoading = $state(false);
	let settingsLoadError = $state<string | null>(null);
	let settingsSaveError = $state<string | null>(null);
	let settingsFlash = $state<string | null>(null);
	let settingsSaving = $state(false);
	let savingKey = $state<keyof AdminSettings | null>(null);

	const isAdmin = $derived(currentUserRole === 'admin');
	const currentCursor = $derived(cursors[pageIndex] ?? null);
	const filteredUsers = $derived.by(() => {
		const needle = query.trim().toLowerCase();
		if (needle.length === 0) return users;
		return users.filter(
			(user) =>
				(user.email ?? '').toLowerCase().includes(needle) ||
				user.displayName.toLowerCase().includes(needle)
		);
	});

	$effect(() => {
		if (!isAdmin || token.length === 0) return;
		// 只在这两处依赖 isAdmin/token：页码游标由翻页按钮推进，交给 untrack 免得同一次翻页触发两次请求。
		untrack(() => {
			cursors = [null];
			pageIndex = 0;
			void loadPage(null);
			void loadSettings();
		});
	});

	function draftFor(user: AdminUser): string {
		return nameDrafts[user.id] ?? user.displayName;
	}

	function roleLabel(role: AdminRole): string {
		return role === 'admin' ? '管理员（admin）' : '成员（member）';
	}

	function settingLabel(key: keyof AdminSettings): string {
		return key === 'registrationOpen' ? '开放注册' : '邮箱验证必需';
	}

	function settingsPatch(key: keyof AdminSettings, value: boolean): AdminSettingsUpdate {
		return key === 'registrationOpen'
			? { registrationOpen: value }
			: { emailVerificationRequired: value };
	}

	async function loadPage(cursor: string | null): Promise<void> {
		listLoading = true;
		listError = null;
		rowError = null;
		try {
			const page = await listAdminUsers(token, { limit: PAGE_SIZE, cursor });
			users = page.users;
			nextCursor = page.nextCursor;
			rowBusy = {};
			nameDrafts = {};
		} catch (error) {
			users = [];
			nextCursor = null;
			listError = describeError(error);
		} finally {
			listLoading = false;
		}
	}

	async function loadSettings(): Promise<void> {
		settingsLoading = true;
		settingsLoadError = null;
		settingsSaveError = null;
		try {
			settings = await fetchAdminSettings(token);
		} catch (error) {
			settings = null;
			settingsLoadError = describeError(error);
		} finally {
			settingsLoading = false;
		}
	}

	function reload(): void {
		listFlash = null;
		void loadPage(currentCursor);
	}

	function goNext(): void {
		if (nextCursor === null) return;
		const trimmed = cursors.slice(0, pageIndex + 1);
		cursors = [...trimmed, nextCursor];
		pageIndex = pageIndex + 1;
		listFlash = null;
		void loadPage(nextCursor);
	}

	function goPrev(): void {
		if (pageIndex === 0) return;
		pageIndex = pageIndex - 1;
		listFlash = null;
		void loadPage(cursors[pageIndex] ?? null);
	}

	async function patchUser(
		user: AdminUser,
		patch: AdminUserUpdate,
		successMessage: string,
		revert?: () => void
	): Promise<void> {
		rowBusy = { ...rowBusy, [user.id]: true };
		rowError = null;
		listFlash = null;
		try {
			const updated = await updateAdminUser(token, user.id, patch);
			if (updated) {
				users = users.map((row) => (row.id === user.id ? updated : row));
				listFlash = successMessage;
			} else {
				listFlash = `${successMessage}（响应体未带用户，已重拉列表）`;
				await loadPage(currentCursor);
			}
		} catch (error) {
			rowError = { id: user.id, message: describeError(error) };
			revert?.();
		} finally {
			const next = { ...rowBusy };
			delete next[user.id];
			rowBusy = next;
		}
	}

	async function changeRole(user: AdminUser, select: HTMLSelectElement): Promise<void> {
		const role = select.value;
		if (role !== 'admin' && role !== 'member') {
			select.value = user.role;
			return;
		}
		if (role === user.role) return;
		try {
			await patchUser(user, { role }, `已把 ${user.email ?? user.id} 的角色改为 ${role}。`);
		} finally {
			if (users.find((row) => row.id === user.id)?.role !== role) select.value = user.role;
		}
	}

	async function toggleDisabled(user: AdminUser): Promise<void> {
		const next = !user.disabled;
		await patchUser(user, { disabled: next }, `已${next ? '停用' : '启用'} ${user.email ?? user.id}。`);
	}

	async function saveDisplayName(user: AdminUser): Promise<void> {
		const displayName = draftFor(user).trim();
		if (displayName.length === 0) {
			rowError = { id: user.id, message: '显示名不能为空。' };
			return;
		}
		await patchUser(user, { displayName }, `已更新 ${user.email ?? user.id} 的显示名。`);
		const next = { ...nameDrafts };
		delete next[user.id];
		nameDrafts = next;
	}

	async function submitCreate(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		createError = null;
		createFlash = null;
		const email = newEmail.trim().toLowerCase();
		if (email.length === 0) {
			createError = '请填写邮箱。';
			return;
		}
		if (newPassword.length === 0) {
			createError = '请填写初始密码。';
			return;
		}
		createBusy = true;
		try {
			const created = await createAdminUser(token, { email, password: newPassword, role: newRole });
			createFlash = created
				? `已创建 ${created.email ?? created.id}（${created.role}）。`
				: '账号已创建，响应体未带用户信息。';
			newEmail = '';
			newPassword = '';
			newRole = 'member';
			await loadPage(currentCursor);
		} catch (error) {
			createError = describeError(error);
		} finally {
			createBusy = false;
		}
	}

	async function toggleSetting(key: keyof AdminSettings): Promise<void> {
		if (!settings || settingsSaving) return;
		const target = !settings[key];
		savingKey = key;
		settingsSaving = true;
		settingsSaveError = null;
		settingsFlash = null;
		try {
			const updated = await updateAdminSettings(token, settingsPatch(key, target));
			settings = updated;
			settingsFlash = `${settingLabel(key)} 已${updated[key] ? '开启' : '关闭'}（服务端已确认）。`;
		} catch (error) {
			settingsSaveError = describeError(error);
		} finally {
			settingsSaving = false;
			savingKey = null;
		}
	}
</script>

{#if !isAdmin}
	<section class="page">
		<header class="intro">
			<span class="badge" data-tone="warn">403</span>
			<h1>需要管理员权限</h1>
			<p>
				当前账号的角色是 <code class="mono">{currentUserRole || '未知'}</code>，管理台只对
				<code class="mono">role=admin</code> 开放。非管理员访问
				<code class="mono">/api/v2/admin/*</code> 时后端返回
				<code class="mono">403 code=forbidden</code>，所以这里不发起请求，也不跳转登录页。
			</p>
		</header>
		<div class="panel narrow">
			<p>如果你认为这是配置错误，请让管理员在管理台把你的角色改成 admin。</p>
			<a class="btn" href="#/">返回任务台</a>
		</div>
	</section>
{:else}
	<section class="page">
		<header class="intro">
			<span class="badge" data-tone="info">admin</span>
			<h1>管理台</h1>
			<p>
				用户与实例设置直接读 <code class="mono">/api/v2/admin/*</code>；所有写入都以后端返回为准，不做本地乐观更新。
			</p>
		</header>

		<section class="panel">
			<header class="panel-head">
				<h2>用户</h2>
				<div class="tools">
					<input
						class="input search"
						type="search"
						placeholder="在当前页筛选：邮箱或显示名"
						aria-label="在当前页筛选用户"
						bind:value={query}
					/>
					<button class="btn" type="button" onclick={reload} disabled={listLoading}>
						{listLoading ? '刷新中…' : '刷新'}
					</button>
				</div>
			</header>

			{#if listError}
				<p class="alert" role="alert">加载用户失败：{listError}</p>
			{/if}
			{#if listFlash}
				<p class="ok-line" role="status">{listFlash}</p>
			{/if}

			{#if listLoading && users.length === 0}
				<div class="empty"><p>正在加载用户列表…</p></div>
			{:else if users.length === 0 && !listError}
				<div class="empty"><p>这一页没有用户。</p></div>
			{:else if users.length > 0}
				<div class="table-wrap">
					<table>
						<thead>
							<tr>
								<th scope="col">邮箱</th>
								<th scope="col">显示名</th>
								<th scope="col">角色</th>
								<th scope="col">状态</th>
								<th scope="col">创建时间</th>
								<th scope="col">操作</th>
							</tr>
						</thead>
						<tbody>
							{#each filteredUsers as user (user.id)}
								{@const isSelf = user.id === currentUserId}
								{@const busy = rowBusy[user.id] === true}
								{@const selfHint = '本人账号：不能改自己的角色、也不能停用自己（后端会返回 409），这里直接禁用。'}
								<tr class:is-self={isSelf}>
									<td class="mono email">{user.email ?? '—'}</td>
									<td>
										<div class="name-cell">
											<input
												class="input name-input"
												value={draftFor(user)}
												aria-label={`显示名（${user.email ?? user.id}）`}
												disabled={busy}
												oninput={(event) => {
													nameDrafts = { ...nameDrafts, [user.id]: event.currentTarget.value };
												}}
											/>
											<button
												class="btn"
												type="button"
												disabled={busy || draftFor(user).trim() === user.displayName}
												onclick={() => saveDisplayName(user)}
											>
												保存
											</button>
										</div>
									</td>
									<td>
										<select
											class="input role-select"
											value={user.role}
											disabled={busy || isSelf}
											title={isSelf ? selfHint : ''}
											aria-label={`角色（${user.email ?? user.id}）`}
											onchange={(event) => changeRole(user, event.currentTarget)}
										>
											{#each ADMIN_ROLES as role (role)}
												<option value={role}>{roleLabel(role)}</option>
											{/each}
										</select>
									</td>
									<td>
										<span class="badge" data-tone={user.disabled ? 'err' : 'ok'}>
											{user.disabled ? '已停用' : '正常'}
										</span>
									</td>
									<td class="mono when">{formatModified(user.createdAt)}</td>
									<td>
										<button
											class="btn"
											type="button"
											disabled={busy || isSelf}
											title={isSelf ? selfHint : ''}
											onclick={() => toggleDisabled(user)}
										>
											{busy ? '保存中…' : user.disabled ? '启用' : '停用'}
										</button>
									</td>
								</tr>
								{#if isSelf}
									<tr class="reason-row">
										<td colspan="6">{selfHint}</td>
									</tr>
								{/if}
								{#if rowError?.id === user.id}
									<tr class="error-row">
										<td colspan="6">
											<p class="alert" role="alert">操作失败：{rowError.message}</p>
										</td>
									</tr>
								{/if}
							{:else}
								<tr>
									<td colspan="6" class="no-match">当前页没有匹配「{query}」的用户。</td>
								</tr>
							{/each}
						</tbody>
					</table>
				</div>

				<footer class="pager">
					<span>
						第 {pageIndex + 1} 页 · 本页 {users.length} 人 · 筛选后 {filteredUsers.length} 人
						{#if nextCursor === null}
							· 没有更多页
						{/if}
					</span>
					<div class="pager-actions">
						<button class="btn" type="button" onclick={goPrev} disabled={pageIndex === 0 || listLoading}>
							上一页
						</button>
						<button
							class="btn"
							type="button"
							onclick={goNext}
							disabled={nextCursor === null || listLoading}
						>
							下一页
						</button>
					</div>
				</footer>
			{/if}
		</section>

		<section class="panel">
			<header class="panel-head">
				<h2>新建账号</h2>
			</header>
			<form class="create" onsubmit={submitCreate}>
				<label class="field">
					<span>邮箱</span>
					<input
						class="input"
						type="email"
						autocomplete="off"
						placeholder="member@example.com"
						bind:value={newEmail}
					/>
				</label>
				<label class="field">
					<span>初始密码</span>
					<input
						class="input"
						type="password"
						autocomplete="new-password"
						placeholder="••••••••"
						bind:value={newPassword}
					/>
				</label>
				<label class="field">
					<span>角色</span>
					<select
						class="input"
						value={newRole}
						onchange={(event) => {
							const value = event.currentTarget.value;
							if (value === 'admin' || value === 'member') newRole = value;
						}}
					>
						{#each ADMIN_ROLES as role (role)}
							<option value={role}>{roleLabel(role)}</option>
						{/each}
					</select>
				</label>
				<button class="btn" data-variant="primary" type="submit" disabled={createBusy}>
					{createBusy ? '正在创建…' : '创建账号'}
				</button>
			</form>
			{#if createError}
				<p class="alert" role="alert">创建失败：{createError}</p>
			{/if}
			{#if createFlash}
				<p class="ok-line" role="status">{createFlash}</p>
			{/if}
		</section>

		<section class="panel">
			<header class="panel-head">
				<h2>实例设置</h2>
				<button class="btn" type="button" onclick={loadSettings} disabled={settingsLoading}>
					{settingsLoading ? '读取中…' : '重新读取'}
				</button>
			</header>

			{#if settingsLoadError}
				<p class="alert" role="alert">读取实例设置失败：{settingsLoadError}</p>
			{/if}
			{#if settingsSaveError}
				<p class="alert" role="alert">保存实例设置失败：{settingsSaveError}</p>
			{/if}
			{#if settingsFlash}
				<p class="ok-line" role="status">{settingsFlash}</p>
			{/if}

			{#if settingsLoading && !settings}
				<div class="empty"><p>正在读取实例设置…</p></div>
			{:else if settings}
				<div class="switch-row">
					<div>
						<p class="switch-title">开放注册</p>
						<p class="switch-hint">
							<code class="mono">registrationOpen</code> · 关闭后新用户不能自助注册。
						</p>
					</div>
					<button
						class="switch"
						type="button"
						role="switch"
						aria-checked={settings.registrationOpen}
						aria-label="开放注册"
						disabled={settingsSaving}
						onclick={() => toggleSetting('registrationOpen')}
					>
						<span class="knob"></span>
					</button>
				</div>
				<div class="switch-row">
					<div>
						<p class="switch-title">邮箱验证必需</p>
						<p class="switch-hint">
							<code class="mono">emailVerificationRequired</code> · 开启后未验证邮箱的账号受限。
						</p>
					</div>
					<button
						class="switch"
						type="button"
						role="switch"
						aria-checked={settings.emailVerificationRequired}
						aria-label="邮箱验证必需"
						disabled={settingsSaving}
						onclick={() => toggleSetting('emailVerificationRequired')}
					>
						<span class="knob"></span>
					</button>
				</div>
				<p class="hint">
					{savingKey ? `正在保存 ${settingLabel(savingKey)}…` : '切换立即 PATCH，收到 200 后才更新上面的开关；失败保留原值并显示服务端消息。'}
				</p>
			{:else if !settingsLoadError}
				<div class="empty"><p>实例设置为空。</p></div>
			{/if}
		</section>
	</section>
{/if}

<style>
	.page {
		display: flex;
		flex-direction: column;
		gap: 18px;
		padding: 28px 32px 40px;
		height: 100vh;
		overflow-y: auto;
		max-width: 1080px;
	}

	.intro {
		display: flex;
		flex-direction: column;
		gap: 8px;
		align-items: flex-start;
	}

	h1 {
		font-size: 20px;
		letter-spacing: -0.01em;
	}

	.intro p {
		font-size: 12.5px;
		color: var(--muted-foreground);
		font-weight: 400;
		line-height: 1.65;
	}

	code {
		font-size: 11.5px;
	}

	.panel {
		display: flex;
		flex-direction: column;
		gap: 12px;
		padding: 16px 18px;
		border-radius: var(--radius-lg);
		border: 1px solid var(--border);
		background: oklch(1 0 0 / 3%);
	}

	.panel.narrow {
		align-items: flex-start;
	}

	.panel-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		flex-wrap: wrap;
	}

	h2 {
		font-size: 14px;
		letter-spacing: 0.02em;
	}

	.tools {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.search {
		width: 240px;
		height: 34px;
	}

	.table-wrap {
		overflow-x: auto;
	}

	table {
		width: 100%;
		border-collapse: collapse;
		font-size: 12.5px;
	}

	th {
		text-align: left;
		font-size: 11px;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--muted-foreground);
		padding: 6px 8px;
		border-bottom: 1px solid var(--border);
		white-space: nowrap;
	}

	td {
		padding: 8px;
		border-bottom: 1px solid oklch(1 0 0 / 6%);
		vertical-align: middle;
	}

	tr.is-self td {
		background: color-mix(in oklch, var(--info) 7%, transparent);
	}

	.email {
		max-width: 220px;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.when {
		color: var(--muted-foreground);
		white-space: nowrap;
	}

	.name-cell {
		display: flex;
		gap: 6px;
		align-items: center;
	}

	.name-input {
		height: 30px;
		width: 150px;
		padding: 0 8px;
	}

	.role-select {
		height: 30px;
		padding: 0 6px;
		min-width: 150px;
	}

	.reason-row td,
	.error-row td {
		padding: 0 8px 8px;
		border-bottom: 1px solid oklch(1 0 0 / 6%);
		font-size: 11.5px;
		color: var(--muted-foreground);
	}

	.no-match {
		padding: 18px 8px;
		color: var(--muted-foreground);
		text-align: center;
	}

	.pager {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		flex-wrap: wrap;
		font-size: 11.5px;
		color: var(--muted-foreground);
	}

	.pager-actions {
		display: flex;
		gap: 8px;
	}

	.create {
		display: grid;
		grid-template-columns: 1.2fr 1fr 0.9fr auto;
		gap: 10px;
		align-items: end;
	}

	.create .btn {
		height: 38px;
	}

	.create select.input {
		height: 38px;
	}

	.ok-line {
		padding: 8px 11px;
		border-radius: var(--radius-md);
		border: 1px solid color-mix(in oklch, var(--ok) 40%, transparent);
		background: color-mix(in oklch, var(--ok) 10%, transparent);
		color: oklch(0.9 0.08 145);
		font-size: 12px;
	}

	.switch-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 16px;
		padding: 10px 12px;
		border-radius: var(--radius-md);
		border: 1px solid var(--border);
		background: oklch(1 0 0 / 2%);
	}

	.switch-title {
		font-size: 13px;
		font-weight: 600;
	}

	.switch-hint {
		margin-top: 3px;
		font-size: 11.5px;
		font-weight: 400;
		color: var(--muted-foreground);
	}

	.switch {
		position: relative;
		width: 42px;
		height: 24px;
		flex: none;
		border-radius: 999px;
		border: 1px solid var(--input);
		background: oklch(0.25 0 0);
		transition: background 120ms ease, border-color 120ms ease;
	}

	.switch[aria-checked='true'] {
		background: color-mix(in oklch, var(--ok) 45%, transparent);
		border-color: color-mix(in oklch, var(--ok) 65%, transparent);
	}

	.switch:disabled {
		opacity: 0.55;
		cursor: not-allowed;
	}

	.knob {
		position: absolute;
		top: 2px;
		left: 2px;
		width: 18px;
		height: 18px;
		border-radius: 50%;
		background: var(--foreground);
		transition: transform 120ms ease;
	}

	.switch[aria-checked='true'] .knob {
		transform: translateX(18px);
	}

	.hint {
		font-size: 11px;
		color: var(--muted-foreground);
		line-height: 1.6;
	}

	@media (max-width: 900px) {
		.create {
			grid-template-columns: 1fr;
		}
	}
</style>
