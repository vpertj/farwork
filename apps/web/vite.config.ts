import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

const API_TARGET = 'http://127.0.0.1:7300';

export default defineConfig({
	plugins: [sveltekit()],
	envPrefix: ['VITE_', 'PUBLIC_'],
	server: {
		port: 5173,
		proxy: {
			'/api': { target: API_TARGET, changeOrigin: true },
			'/ws': { target: API_TARGET, ws: true, changeOrigin: true }
		}
	}
});
