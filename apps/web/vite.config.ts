import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

const DEFAULT_API_TARGET = 'http://127.0.0.1:7300';

// 本仓未装 @types/node，vite.config.ts 又要过 svelte-check：从 globalThis 上取 process.env
const env = (globalThis as { process?: { env?: Record<string, string | undefined> } }).process?.env ?? {};

export default defineConfig(({ command }) => {
	const apiTarget = (env.FARWORK_DEV_API?.trim() || DEFAULT_API_TARGET).replace(/\/+$/, '');
	if (command === 'serve') {
		// 并行任务曾各自抢 7300 顶替演示环境，启动就把代理目标打出来
		console.log(`[farwork] dev proxy -> ${apiTarget}`);
	}
	return {
		plugins: [sveltekit()],
		envPrefix: ['VITE_', 'PUBLIC_'],
		server: {
			port: 5173,
			proxy: {
				'/api': { target: apiTarget, ws: true, changeOrigin: true },
				'/ws': { target: apiTarget, ws: true, changeOrigin: true }
			}
		}
	};
});
