<script lang="ts">
	import { workspace } from '$lib/workspace.svelte';
	import { createAndStartSession } from '$lib/api/workspace';
	import { describeError } from '$lib/format';
	import { navigate } from '$lib/router';
	import {
		runtimeLabel,
		SUPPORTED_RUNTIMES,
		type SupportedRuntime
	} from '$lib/timeline';
	import type { ConnectorResult } from '$lib/api/types';

	let { projectId, token }: { projectId: string | null; token: string } = $props();

	let connectorId = $state('');
	let chosenProjectId = $state('');
	let runtime = $state<SupportedRuntime>('codex');
	let content = $state('');
	let busy = $state(false);
	let errorMessage = $state<string | null>(null);
	let connectorResult = $state<ConnectorResult | null>(null);
	let createdSessionId = $state<string | null>(null);
	let appliedPreset = '';

	const connectors = $derived(workspace.connectors);
	const connector = $derived(connectors.find((row) => row.id === connectorId) ?? null);
	const projects = $derived(workspace.projects.filter((row) => row.connectorId === connectorId));
	const project = $derived(projects.find((row) => row.id === chosenProjectId) ?? null);
	const undispatched = $derived(connectorResult !== null && !connectorResult.ok);

	$effect(() => {
		const key = projectId ?? '';
		if (key === appliedPreset) return;
		if (key.length === 0) {
			appliedPreset = key;
			return;
		}
		const preset = workspace.projects.find((row) => row.id === key);
		if (!preset) return;
		appliedPreset = key;
		connectorId = preset.connectorId;
		chosenProjectId = preset.id;
	});

	function selectConnector(event: Event): void {
		const target = event.currentTarget;
		if (!(target instanceof HTMLSelectElement)) return;
		connectorId = target.value;
		chosenProjectId = '';
		connectorResult = null;
		createdSessionId = null;
		errorMessage = null;
	}

	function selectProject(event: Event): void {
		const target = event.currentTarget;
		if (!(target instanceof HTMLSelectElement)) return;
		chosenProjectId = target.value;
		connectorResult = null;
		createdSessionId = null;
		errorMessage = null;
	}

	async function submit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		errorMessage = null;
		connectorResult = null;
		createdSessionId = null;
		if (connectorId.length === 0) {
			errorMessage = '请选择设备（连接器）。';
			return;
		}
		if (chosenProjectId.length === 0) {
			errorMessage = '请选择项目。';
			return;
		}
		const text = content.trim();
		if (text.length === 0) {
			errorMessage = '请输入首条消息。';
			return;
		}
		busy = true;
		try {
			const result = await createAndStartSession(token, {
				connectorId,
				projectId: chosenProjectId,
				runtime,
				content: text,
				clientMessageId: `cmid-${crypto.randomUUID()}`
			});
			createdSessionId = result.session.id;
			connectorResult = result.connectorResult;
			if (result.connectorResult?.ok) {
				void workspace.load(token);
				navigate({ page: 'session', sessionId: result.session.id });
			}
		} catch (error) {
			errorMessage = describeError(error);
		} finally {
			busy = false;
		}
	}

	function failureTitle(result: ConnectorResult): string {
		if (result.code === 'connector_offline') return '设备未连接：指令未下发';
		return result.code ? `指令未下发（${result.code}）` : '指令未下发';
	}
</script>

<section class="new-session">
	<header>
		<a class="back" href="#/" title="返回任务台">←</a>
		<div>
			<h1>新建会话</h1>
			<p>选设备与项目，指定运行时会话，写下第一条消息后由 <code class="mono">POST /sessions/create-and-start</code> 下发。</p>
		</div>
	</header>

	{#if workspace.error}
		<p class="alert" role="alert">{workspace.error}</p>
	{/if}

	{#if workspace.loading && connectors.length === 0}
		<div class="empty"><p>正在加载设备与项目…</p></div>
	{:else if connectors.length === 0}
		<div class="empty">
			<p>还没有任何设备（connector）。</p>
			<p>设备通过 connector 出站连上 hub 后，才会出现在这里。</p>
		</div>
	{:else}
		<form onsubmit={submit}>
			<label class="field">
				<span>设备（connector）</span>
				<select class="input" value={connectorId} onchange={selectConnector}>
					<option value="">请选择设备</option>
					{#each connectors as row (row.id)}
						<option value={row.id}>{row.name} · {row.status}</option>
					{/each}
				</select>
			</label>

			{#if connector}
				<p class="note-line">
					设备状态：<strong>{connector.status}</strong>
					{#if connector.status !== 'online'}
						· 该设备当前不在线，create-and-start 会返回 <code class="mono">connectorResult.ok=false</code>，指令不会下发。
					{/if}
				</p>
			{/if}

			{#if connectorId.length > 0}
				<label class="field">
					<span>项目</span>
					{#if projects.length === 0}
						<select class="input" disabled>
							<option value="">该设备下还没有项目</option>
						</select>
					{:else}
						<select class="input" value={chosenProjectId} onchange={selectProject}>
							<option value="">请选择项目</option>
							{#each projects as row (row.id)}
								<option value={row.id}>{row.name}</option>
							{/each}
						</select>
					{/if}
				</label>
			{/if}

			{#if project}
				<p class="note-line">工作目录：<code class="mono">{project.workspacePath}</code></p>
			{/if}

			<label class="field">
				<span>运行时</span>
				<select
					class="input"
					value={runtime}
					onchange={(event) => {
						const target = event.currentTarget;
						if (target instanceof HTMLSelectElement) {
							const next = SUPPORTED_RUNTIMES.find((code) => code === target.value);
							if (next) runtime = next;
						}
					}}
				>
					{#each SUPPORTED_RUNTIMES as code (code)}
						<option value={code}>{runtimeLabel(code)} · {code}</option>
					{/each}
				</select>
			</label>

			<label class="field">
				<span>首条消息</span>
				<textarea
					class="input textarea"
					rows="4"
					placeholder="描述要这个 agent 做什么…"
					bind:value={content}
				></textarea>
			</label>

			{#if errorMessage}
				<p class="alert" role="alert">{errorMessage}</p>
			{/if}

			<button class="btn" data-variant="primary" type="submit" disabled={busy}>
				{busy ? '正在创建并下发…' : '创建并启动会话'}
			</button>
		</form>

		{#if undispatched && connectorResult}
			<div class="failure" role="alert">
				<p class="failure-title">{failureTitle(connectorResult)}</p>
				<p>运行时没有启动，这个会话不会自己跑起来。</p>
				<ul>
					{#if createdSessionId}
						<li>
							会话已创建（首条消息已入库）：
							<a href="#/session/{encodeURIComponent(createdSessionId)}" class="mono">{createdSessionId}</a>
						</li>
					{/if}
					<li>返回码：<code class="mono">{connectorResult.code ?? 'null'}</code></li>
				</ul>
				<p class="hint">设备上线后可在该会话页继续输入，或从任务台重新发起新的会话。</p>
			</div>
		{/if}
	{/if}
</section>

<style>
	.new-session {
		display: flex;
		flex-direction: column;
		gap: 16px;
		padding: 28px 32px;
		max-width: 720px;
		height: 100vh;
		overflow-y: auto;
	}

	header {
		display: flex;
		align-items: flex-start;
		gap: 10px;
	}

	.back {
		display: grid;
		place-items: center;
		width: 28px;
		height: 28px;
		border-radius: var(--radius-sm);
		color: var(--muted-foreground);
		flex: none;
	}

	.back:hover {
		background: oklch(1 0 0 / 8%);
		color: var(--foreground);
	}

	h1 {
		font-size: 20px;
		letter-spacing: -0.01em;
	}

	header p {
		margin-top: 6px;
		font-size: 12.5px;
		color: var(--muted-foreground);
		font-weight: 400;
		line-height: 1.6;
	}

	form {
		display: flex;
		flex-direction: column;
		gap: 12px;
	}

	select.input {
		height: 38px;
	}

	.textarea {
		height: auto;
		padding: 10px 12px;
		resize: vertical;
		min-height: 96px;
		line-height: 1.6;
	}

	.note-line {
		font-size: 11.5px;
		color: var(--muted-foreground);
		margin-top: -4px;
	}

	.failure {
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 14px 16px;
		border-radius: var(--radius-md);
		border: 1px solid color-mix(in oklch, var(--err) 45%, transparent);
		background: color-mix(in oklch, var(--err) 12%, transparent);
		font-size: 12.5px;
		line-height: 1.6;
	}

	.failure-title {
		font-weight: 600;
		color: oklch(0.92 0.05 22);
	}

	.failure ul {
		margin: 0;
		padding-left: 18px;
	}

	.hint {
		font-size: 11px;
		color: var(--muted-foreground);
	}

	code {
		font-size: 11.5px;
	}
</style>
