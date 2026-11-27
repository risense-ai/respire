// Synthetic vectors generated independently of the Rust implementation.
// Requires Node.js with crypto.argon2Sync; never use real account credentials.
import * as crypto from 'node:crypto';
import { writeFileSync } from 'node:fs';

const salt = Buffer.from('000102030405060708090a0b0c0d0e0f', 'hex');
const nonce = Buffer.from('101112131415161718191a1b', 'hex');
const urk = Buffer.alloc(32, 42);
const login = 'synthetic-login';
const superPassword = 'synthetic-super';
const accountSecret = '11'.repeat(32);
const secretKey = 'A3-000102-030405-060708-090a0b-0c0d0e-0f1011';
const hkdf = (ikm, salt, info, length = 32) => Buffer.from(
  crypto.hkdfSync('sha256', ikm, salt, info, length));
const encrypt = (key, plain) => {
  const cipher = crypto.createCipheriv('aes-256-gcm', key, nonce);
  return Buffer.concat([cipher.update(plain), cipher.final(), cipher.getAuthTag()]).toString('hex');
};
const passwordKey = crypto.argon2Sync('argon2id', {
  message: login, nonce: salt, parallelism: 1, memory: 19456, passes: 2, tagLength: 32,
});
const superKey = crypto.pbkdf2Sync(superPassword, salt, 210000, 32, 'sha256');
const vectors = [];
for (const namespace of ['onememory', 'rsrs']) {
  const prefix = namespace === 'rsrs' ? 'rsrs:v1:' : '';
  const dataKey = hkdf(urk, Buffer.alloc(0), `${namespace}:data:v1`);
  const embeddingKey = hkdf(urk, Buffer.alloc(0), `${namespace}:embedding:v1`);
  const authSalt = hkdf('golden-user', Buffer.alloc(0), `${namespace}:auth-salt:v1`, 16);
  for (const version of [1, 2, 3, 4]) {
    const kek = version === 1 ? hkdf(passwordKey, accountSecret, `${namespace}:kek:v1`)
      : version === 2 ? superKey
      : version === 3 ? hkdf(superKey, secretKey, `${namespace}:kek:v1`)
      : hkdf(Buffer.from('000102030405060708090a0b0c0d0e0f1011', 'hex'), salt, `${namespace}:kek:v4`);
    vectors.push({ namespace, version, kek: kek.toString('hex'),
      wrapped_urk: prefix + encrypt(kek, urk), nonce: nonce.toString('hex'),
      ciphertext: prefix + encrypt(dataKey, Buffer.from('Golden legacy memory.')),
      data_key: dataKey.toString('hex'), embedding_key: embeddingKey.toString('hex'),
      embedding: encrypt(embeddingKey, Buffer.from([0, 0, 128, 63])),
      auth_salt: prefix + authSalt.toString('hex'),
      auth_hash: crypto.pbkdf2Sync(login, authSalt, 100000, 32, 'sha256').toString('hex'),
    });
  }
}
writeFileSync(new URL('../crates/crypto/fixtures/vault-v1-v4.json', import.meta.url),
  JSON.stringify({ provenance: 'Node.js crypto: Argon2id, PBKDF2-SHA256, HKDF-SHA256, AES-256-GCM',
    salt: salt.toString('hex'), login, super_password: superPassword, account_secret: accountSecret,
    secret_key: secretKey, urk: urk.toString('hex'), vectors }, null, 2) + '\n');
