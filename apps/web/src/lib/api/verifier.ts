const PBKDF2_ITERATIONS = 120_000;
const VERIFIER_BYTES = 32;

export async function derivePasswordVerifier(password: string, salt: string): Promise<string> {
	const subtle = globalThis.crypto?.subtle;
	if (!subtle) {
		throw new Error('当前页面无法使用 WebCrypto（需要 HTTPS 或 localhost），无法计算密码校验值。');
	}
	const passwordBytes = new TextEncoder().encode(password);
	const saltBytes = new TextEncoder().encode(salt);
	try {
		const key = await subtle.importKey('raw', passwordBytes, 'PBKDF2', false, ['deriveBits']);
		const bits = await subtle.deriveBits(
			{ name: 'PBKDF2', hash: 'SHA-256', salt: saltBytes, iterations: PBKDF2_ITERATIONS },
			key,
			VERIFIER_BYTES * 8
		);
		return base64Url(new Uint8Array(bits));
	} finally {
		passwordBytes.fill(0);
	}
}

function base64Url(bytes: Uint8Array): string {
	let binary = '';
	for (const byte of bytes) binary += String.fromCharCode(byte);
	return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
