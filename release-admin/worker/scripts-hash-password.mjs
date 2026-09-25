// Generate a PBKDF2 hash for inserting an initial administrator.
// Usage: node scripts-hash-password.mjs 'a-long-password'
const password = process.argv[2];
if (!password || password.length < 12) throw new Error('password must be at least 12 characters');
const bytes = crypto.getRandomValues(new Uint8Array(16));
const key = await crypto.subtle.importKey('raw', new TextEncoder().encode(password), 'PBKDF2', false, ['deriveBits']);
const bits = await crypto.subtle.deriveBits({ name: 'PBKDF2', salt: bytes, iterations: 120000, hash: 'SHA-256' }, key, 256);
const encode = value => Buffer.from(value instanceof ArrayBuffer ? new Uint8Array(value) : value).toString('base64url');
console.log(`pbkdf2$120000$${encode(bytes)}$${encode(bits)}`);
