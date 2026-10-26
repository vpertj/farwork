<script lang="ts">
	import { auth } from '$lib/auth.svelte';
	import { navigate } from '$lib/router';

	let { mode }: { mode: 'login' | 'register' } = $props();

	let email = $state('');
	let password = $state('');
	let localError = $state<string | null>(null);

	const config = $derived(auth.config);
	const message = $derived(localError ?? auth.error);

	async function submit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		localError = null;
		if (email.trim().length === 0 || password.length === 0) {
			localError = '请填写邮箱与密码。';
			return;
		}
		const ok = await auth.signIn(email, password);
		if (ok) navigate({ page: 'home' });
	}

	function switchMode(): void {
		localError = null;
		navigate({ page: mode === 'login' ? 'register' : 'login' });
	}
</script>

<div class="auth">
	<div class="card">
		<header>
			<div class="brand">
				<span class="glyph" aria-hidden="true">◈</span>
				<span>Farwork</span>
			</div>
			<h1>{mode === 'login' ? '登录工作台' : '注册账号'}</h1>
			<p class="sub">
				{mode === 'login'
					? '使用邮箱与密码登录，随后由 Rust 网关代理到上游会话服务。'
					: '注册流程尚未接入本轮切片，请先使用已有账号登录。'}
			</p>
		</header>

		{#if config?.needsBootstrap}
			<p class="note">当前实例尚未初始化：需要服务端首启的 bootstrap 令牌才能创建管理员账号。</p>
		{/if}
		{#if config && !config.registrationOpen}
			<p class="note">该实例已关闭自助注册。</p>
		{/if}

		{#if mode === 'login'}
			<form onsubmit={submit}>
				<label class="field">
					<span>邮箱</span>
					<input
						class="input"
						type="email"
						autocomplete="username"
						placeholder="you@example.com"
						bind:value={email}
					/>
				</label>
				<label class="field">
					<span>密码</span>
					<input
						class="input"
						type="password"
						autocomplete="current-password"
						placeholder="••••••••"
						bind:value={password}
					/>
				</label>
				{#if message}
					<p class="alert" role="alert">{message}</p>
				{/if}
				<button class="btn submit" type="submit" data-variant="primary" disabled={auth.busy}>
					{auth.busy ? '正在登录…' : '登录'}
				</button>
			</form>
		{:else}
			<div class="empty">
				<p>注册入口未实现。</p>
				<p>上游 <code class="mono">POST /api/v2/auth/register</code> 需要密码校验值与可选邀请码。</p>
			</div>
		{/if}

		<footer>
			<button class="btn" data-variant="ghost" type="button" onclick={switchMode}>
				{mode === 'login' ? '没有账号？了解注册' : '返回登录'}
			</button>
		</footer>
	</div>
</div>

<style>
	.auth {
		min-height: 100vh;
		display: grid;
		place-items: center;
		padding: 32px 20px;
		background:
			radial-gradient(circle at 20% 0%, oklch(0.32 0.06 265 / 45%), transparent 55%),
			var(--background);
	}

	.card {
		width: min(420px, 100%);
		display: flex;
		flex-direction: column;
		gap: 18px;
		padding: 28px;
		border-radius: var(--radius-xl);
		border: 1px solid var(--border);
		background: color-mix(in oklch, var(--card) 92%, transparent);
		box-shadow: 0 24px 60px oklch(0 0 0 / 45%);
	}

	header {
		display: flex;
		flex-direction: column;
		gap: 8px;
	}

	.brand {
		display: flex;
		align-items: center;
		gap: 8px;
		font-size: 13px;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--muted-foreground);
	}

	.glyph {
		color: var(--info);
		font-size: 16px;
	}

	h1 {
		font-size: 20px;
		letter-spacing: -0.01em;
	}

	.sub {
		font-size: 12.5px;
		color: var(--muted-foreground);
		font-weight: 400;
		line-height: 1.6;
	}

	form {
		display: flex;
		flex-direction: column;
		gap: 14px;
	}

	.note {
		padding: 9px 11px;
		border-radius: var(--radius-md);
		border: 1px solid color-mix(in oklch, var(--warn) 40%, transparent);
		background: color-mix(in oklch, var(--warn) 12%, transparent);
		color: oklch(0.9 0.06 85);
		font-size: 12px;
		font-weight: 400;
	}

	.submit {
		height: 40px;
		width: 100%;
	}

	footer {
		display: flex;
		justify-content: center;
	}

	code {
		font-size: 11.5px;
		color: var(--muted-foreground);
	}
</style>
