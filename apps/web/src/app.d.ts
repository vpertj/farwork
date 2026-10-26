/// <reference types="vite/client" />

interface ImportMetaEnv {
	readonly PUBLIC_FARWORK_API?: string;
}

interface ImportMeta {
	readonly env: ImportMetaEnv;
}

declare global {
	namespace App {}
}

export {};
