"""Vault fixture pins, compatibility crypto, and read-only fixture helpers."""
from native_fixture_process import canonical_scope_path
import hashlib
import hmac
from pathlib import Path
import urllib.request



UPSTREAM = "https://api-dev.rsrs.rs"
REQUIRED = ("model_cpu_real", "local_v1_upgrade", "cloud_v2_recovery_upgrade", "cloud_v3_secret_key_recovery_upgrade")
DATABASE_REQUIRED = ("model_cpu_real", "legacy_ciphertext_sync")
MODEL_HASHES = {
    "onnx/model_quantized.onnx": "0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d",
    "tokenizer.json": "6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790",
}


def require(value, code):
    if not value:
        raise RuntimeError(code)


def same_scope_path(value, expected):
    return (isinstance(value, str) and Path(value).is_absolute() and Path(value).is_dir()
        and canonical_scope_path(value) == canonical_scope_path(expected))


def readonly_database_uri(path):
    # Path.as_uri treats the Windows verbatim prefix as an SQLite URI authority.
    return 'file:' + urllib.request.pathname2url(str(path.resolve(strict=True))) + '?mode=ro'


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def hkdf(ikm, salt, info, length=32):
    prk = hmac.new(salt if salt is not None else bytes(32), ikm, hashlib.sha256).digest()
    return hmac.new(prk, info + b"\x01", hashlib.sha256).digest()[:length]


PREFIX = "rsrs:v1:"


def encrypted_bytes(value):
    return bytes.fromhex(value.removeprefix(PREFIX))


def v4_kek(code, salt, wrapped=""):
    entropy = bytes.fromhex(code.strip().removeprefix("A3-").replace("-", ""))
    require(len(entropy) == 18, "recovery_code_invalid")
    info = b"rsrs:kek:v4" if wrapped.startswith(PREFIX) else b"onememory:kek:v4"
    return hkdf(entropy, bytes.fromhex(salt), info)


def legacy_kek(version, password, account_secret, salt, current=False):
    if version == 1:
        from cryptography.hazmat.primitives.kdf.argon2 import Argon2id
        # Public crypto::derive_kek uses Argon2::default (v19, m19456, t2, p1).
        intermediate = Argon2id(salt=bytes.fromhex(salt), length=32, iterations=2,
            lanes=1, memory_cost=19456).derive(password.encode())
    else:
        intermediate = hashlib.pbkdf2_hmac("sha256", password.encode(), bytes.fromhex(salt), 210000, 32)
    info = b"rsrs:kek:v1" if current else b"onememory:kek:v1"
    return intermediate if version == 2 else hkdf(intermediate, account_secret.encode(), info)


def decrypt_content(urk, ciphertext, nonce):
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    info = b"rsrs:data:v1" if ciphertext.startswith(PREFIX) else b"onememory:data:v1"
    return AESGCM(hkdf(urk, None, info)).decrypt(bytes.fromhex(nonce), encrypted_bytes(ciphertext), None)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None
