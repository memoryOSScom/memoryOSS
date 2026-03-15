// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, KeyInit, OsRng},
};
use rand::RngCore;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use zeroize::Zeroize;

use crate::config::EncryptionConfig;

const KEY_FILE: &str = "memoryoss.key";
const NONCE_SIZE: usize = 12;

// ── KeyProvider trait ──────────────────────────────────────────────────

/// Trait for pluggable key management backends.
/// Provides namespace-scoped data encryption keys (32 bytes for AES-256).
pub trait KeyProvider: Send + Sync {
    /// Get or derive a 32-byte data encryption key for the given namespace.
    fn get_data_key(&self, namespace: &str) -> anyhow::Result<[u8; 32]>;

    /// Persist a rotated key for the given namespace.
    /// Default: no-op (for providers like KMS/Vault that manage keys externally).
    fn store_rotated_key(&self, _namespace: &str, _key: &[u8; 32]) -> anyhow::Result<()> {
        Ok(())
    }
}

// ── Local key provider (default) ───────────────────────────────────────

/// Local file-based key provider with HKDF key derivation.
/// Master Key (local file) → HKDF → Namespace Key → encrypts data.
pub struct LocalKeyProvider {
    #[allow(dead_code)]
    master_key: zeroize::Zeroizing<[u8; 32]>,
    data_dir: PathBuf,
}

impl LocalKeyProvider {
    pub fn load_or_create(data_dir: &Path) -> anyhow::Result<Self> {
        let key_path = data_dir.join(KEY_FILE);
        let key_bytes = if key_path.exists() {
            let bytes = std::fs::read(&key_path)?;
            if bytes.len() != 32 {
                anyhow::bail!("Invalid key file: expected 32 bytes, got {}", bytes.len());
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            key
        } else {
            let mut key = [0u8; 32];
            OsRng.fill_bytes(&mut key);
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut f = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&key_path)?;
                f.write_all(&key)?;
            }
            #[cfg(not(unix))]
            {
                std::fs::write(&key_path, key)?;
            }
            tracing::info!("Generated new encryption key at {}", key_path.display());
            key
        };

        Ok(Self {
            master_key: zeroize::Zeroizing::new(key_bytes),
            data_dir: data_dir.to_path_buf(),
        })
    }

    fn rotated_key_path(&self, namespace: &str) -> PathBuf {
        let safe_ns =
            namespace.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
        self.data_dir.join(format!(".rotated_key_{safe_ns}"))
    }
}

impl KeyProvider for LocalKeyProvider {
    fn get_data_key(&self, namespace: &str) -> anyhow::Result<[u8; 32]> {
        // Check for rotated key file first
        // (stored by store_rotated_key during key rotation)
        let rotated_path = self.rotated_key_path(namespace);
        if rotated_path.exists() {
            let bytes = std::fs::read(&rotated_path)?;
            if bytes.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                return Ok(key);
            }
        }
        // HKDF-SHA256: master_key + namespace → namespace-scoped DEK
        derive_namespace_key(&self.master_key, namespace)
    }

    fn store_rotated_key(&self, namespace: &str, key: &[u8; 32]) -> anyhow::Result<()> {
        let rotated_path = self.rotated_key_path(namespace);
        std::fs::write(&rotated_path, key)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&rotated_path, std::fs::Permissions::from_mode(0o600))?;
        }
        tracing::info!(
            namespace,
            "Rotated key persisted to {}",
            rotated_path.display()
        );
        Ok(())
    }
}

/// A2 FIX: HKDF-SHA256 key derivation (RFC 5869).
fn derive_namespace_key(master: &[u8; 32], namespace: &str) -> anyhow::Result<[u8; 32]> {
    // A1 FIX: validate namespace to prevent path traversal in downstream key paths
    if namespace.contains("..") || namespace.contains('/') || namespace.contains('\\') {
        anyhow::bail!("invalid namespace for key derivation: contains path characters");
    }
    use hkdf::Hkdf;
    use sha2::Sha256;
    let salt = b"memoryoss-ns-key-v2";
    let hk = Hkdf::<Sha256>::new(Some(salt), master);
    let mut key = [0u8; 32];
    hk.expand(namespace.as_bytes(), &mut key)
        .map_err(|e| anyhow::anyhow!("HKDF expand failed: {e}"))?;
    Ok(key)
}

// ── AWS KMS key provider ───────────────────────────────────────────────

/// AWS KMS envelope encryption.
/// Master Key = KMS CMK (never leaves AWS).
/// Namespace Key = GenerateDataKey per namespace, cached locally.
/// Wrapped (encrypted) namespace keys stored in data_dir.
pub struct AwsKmsKeyProvider {
    key_id: String,
    region: String,
    data_dir: PathBuf,
}

impl AwsKmsKeyProvider {
    pub fn new(key_id: String, region: String, data_dir: PathBuf) -> Self {
        Self {
            key_id,
            region,
            data_dir,
        }
    }

    fn wrapped_key_path(&self, namespace: &str) -> PathBuf {
        // A1 FIX: sanitize namespace in file path
        let safe_ns =
            namespace.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
        self.data_dir.join(format!(".kms_key_{safe_ns}.enc"))
    }

    /// A3/A4 FIX: AWS KMS requires SigV4 signing (aws-sdk-kms crate).
    /// This stub returns an error until properly implemented.
    fn generate_data_key(&self, _namespace: &str) -> anyhow::Result<[u8; 32]> {
        anyhow::bail!(
            "AWS KMS key provider is not yet implemented. \
             Use 'local' key provider or add aws-sdk-kms dependency. \
             See: https://docs.rs/aws-sdk-kms"
        )
    }
}

impl KeyProvider for AwsKmsKeyProvider {
    fn get_data_key(&self, _namespace: &str) -> anyhow::Result<[u8; 32]> {
        // AWS KMS is not yet implemented — reading plaintext keys from disk
        // would defeat the purpose of envelope encryption. Fail explicitly.
        anyhow::bail!(
            "AWS KMS key provider is not yet implemented. \
             Use 'local' key provider or add aws-sdk-kms dependency. \
             See: https://docs.rs/aws-sdk-kms"
        )
    }
}

/// AWS KMS GenerateDataKey API call.
async fn kms_generate_data_key(
    key_id: &str,
    region: &str,
    _namespace: &str,
) -> anyhow::Result<Vec<u8>> {
    let endpoint = format!("https://kms.{region}.amazonaws.com");
    let client = reqwest::Client::new();

    let resp = client
        .post(&endpoint)
        .header("Content-Type", "application/x-amz-json-1.1")
        .header("X-Amz-Target", "TrentService.GenerateDataKey")
        .json(&serde_json::json!({
            "KeyId": key_id,
            "KeySpec": "AES_256"
        }))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("AWS KMS GenerateDataKey failed {status}: {body}");
    }

    let body: serde_json::Value = resp.json().await?;
    let plaintext_b64 = body["Plaintext"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing Plaintext in KMS response"))?;

    use sha2::{Digest, Sha256};
    // base64 decode
    let decoded = base64_decode(plaintext_b64)?;
    // Ensure 32 bytes via hash if needed
    if decoded.len() == 32 {
        Ok(decoded)
    } else {
        let mut hasher = Sha256::new();
        hasher.update(&decoded);
        Ok(hasher.finalize().to_vec())
    }
}

fn base64_decode(input: &str) -> anyhow::Result<Vec<u8>> {
    // Simple base64 decode without adding a dependency
    let mut output = Vec::new();
    let chars: Vec<u8> = input
        .bytes()
        .filter(|b| *b != b'\n' && *b != b'\r')
        .collect();

    for chunk in chars.chunks(4) {
        let mut buf = [0u8; 4];
        let mut len = 0;
        for &b in chunk {
            buf[len] = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => break,
                _ => continue,
            };
            len += 1;
        }
        if len >= 2 {
            output.push((buf[0] << 2) | (buf[1] >> 4));
        }
        if len >= 3 {
            output.push((buf[1] << 4) | (buf[2] >> 2));
        }
        if len >= 4 {
            output.push((buf[2] << 6) | buf[3]);
        }
    }
    Ok(output)
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);

        output.push(TABLE[(b0 >> 2) as usize] as char);
        output.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(TABLE[(b2 & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

// ── HashiCorp Vault key provider ───────────────────────────────────────

/// HashiCorp Vault Transit engine for key management.
/// Master Key = Vault Transit key (never leaves Vault).
/// Namespace Key = Vault-encrypted, stored locally.
pub struct VaultKeyProvider {
    address: String,
    token: zeroize::Zeroizing<String>,
    mount: String,
    key_name: String,
    data_dir: PathBuf,
}

impl VaultKeyProvider {
    pub fn new(
        address: String,
        token: String,
        mount: String,
        key_name: String,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            address,
            token: zeroize::Zeroizing::new(token),
            mount,
            key_name,
            data_dir,
        }
    }

    fn wrapped_key_path(&self, namespace: &str) -> PathBuf {
        // A1 FIX: sanitize namespace in file path
        let safe_ns =
            namespace.replace(|c: char| !c.is_alphanumeric() && c != '-' && c != '_', "_");
        self.data_dir.join(format!(".vault_key_{safe_ns}.enc"))
    }

    fn generate_data_key(&self, namespace: &str) -> anyhow::Result<[u8; 32]> {
        let wrapped_path = self.wrapped_key_path(namespace);
        let (plaintext, ciphertext) = vault_generate_data_key(
            &self.address,
            &self.token,
            &self.mount,
            &self.key_name,
            namespace,
        )?;

        // Store the vault-encrypted key
        std::fs::write(&wrapped_path, &ciphertext)?;

        let mut key = [0u8; 32];
        key.copy_from_slice(&plaintext[..32]);
        Ok(key)
    }

    fn unwrap_data_key(&self, wrapped: &[u8]) -> anyhow::Result<[u8; 32]> {
        let ciphertext = String::from_utf8_lossy(wrapped).to_string();
        let plaintext = vault_decrypt(
            &self.address,
            &self.token,
            &self.mount,
            &self.key_name,
            &ciphertext,
        )?;

        let mut key = [0u8; 32];
        key.copy_from_slice(&plaintext[..32]);
        Ok(key)
    }
}

impl KeyProvider for VaultKeyProvider {
    fn get_data_key(&self, namespace: &str) -> anyhow::Result<[u8; 32]> {
        let wrapped_path = self.wrapped_key_path(namespace);

        if wrapped_path.exists() {
            let wrapped = std::fs::read(&wrapped_path)?;
            return self.unwrap_data_key(&wrapped);
        }

        self.generate_data_key(namespace)
    }

    fn store_rotated_key(&self, namespace: &str, key: &[u8; 32]) -> anyhow::Result<()> {
        let wrapped_path = self.wrapped_key_path(namespace);
        let ciphertext =
            vault_encrypt(&self.address, &self.token, &self.mount, &self.key_name, key)?;

        std::fs::write(&wrapped_path, &ciphertext)?;
        Ok(())
    }
}

/// Vault Transit: generate a data key (returns plaintext + ciphertext).
fn vault_generate_data_key(
    address: &str,
    token: &str,
    mount: &str,
    key_name: &str,
    _namespace: &str,
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let url = format!("{address}/v1/{mount}/datakey/plaintext/{key_name}");
    let client = reqwest::blocking::Client::new();

    let resp = client
        .post(&url)
        .header("X-Vault-Token", token)
        .json(&serde_json::json!({"bits": 256}))
        .timeout(std::time::Duration::from_secs(10))
        .send()?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        anyhow::bail!("Vault datakey generation failed {status}: {body}");
    }

    let body: serde_json::Value = resp.json()?;
    let plaintext_b64 = body["data"]["plaintext"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing plaintext in Vault response"))?;
    let ciphertext = body["data"]["ciphertext"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing ciphertext in Vault response"))?;

    let plaintext = base64_decode(plaintext_b64)?;
    Ok((plaintext, ciphertext.as_bytes().to_vec()))
}

/// Vault Transit: decrypt a ciphertext.
fn vault_decrypt(
    address: &str,
    token: &str,
    mount: &str,
    key_name: &str,
    ciphertext: &str,
) -> anyhow::Result<Vec<u8>> {
    let url = format!("{address}/v1/{mount}/decrypt/{key_name}");
    let client = reqwest::blocking::Client::new();

    let resp = client
        .post(&url)
        .header("X-Vault-Token", token)
        .json(&serde_json::json!({"ciphertext": ciphertext}))
        .timeout(std::time::Duration::from_secs(10))
        .send()?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        anyhow::bail!("Vault decrypt failed {status}: {body}");
    }

    let body: serde_json::Value = resp.json()?;
    let plaintext_b64 = body["data"]["plaintext"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing plaintext in Vault decrypt response"))?;

    base64_decode(plaintext_b64)
}

/// Vault Transit: encrypt a plaintext payload and return the wrapped ciphertext.
fn vault_encrypt(
    address: &str,
    token: &str,
    mount: &str,
    key_name: &str,
    plaintext: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let url = format!("{address}/v1/{mount}/encrypt/{key_name}");
    let client = reqwest::blocking::Client::new();

    let resp = client
        .post(&url)
        .header("X-Vault-Token", token)
        .json(&serde_json::json!({"plaintext": base64_encode(plaintext)}))
        .timeout(std::time::Duration::from_secs(10))
        .send()?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        anyhow::bail!("Vault encrypt failed {status}: {body}");
    }

    let body: serde_json::Value = resp.json()?;
    let ciphertext = body["data"]["ciphertext"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing ciphertext in Vault encrypt response"))?;

    Ok(ciphertext.as_bytes().to_vec())
}

// ── Encryptor (uses KeyProvider) ───────────────────────────────────────

/// A retired key kept around for grace-period decryption.
struct RetiredKey {
    id: String,
    cipher: Aes256Gcm,
    retired_at: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
}

pub struct Encryptor {
    key_provider: Box<dyn KeyProvider>,
    /// Cache of namespace → current AES cipher.
    cipher_cache: RwLock<HashMap<String, Aes256Gcm>>,
    /// Retired keys per namespace, kept for grace-period decryption.
    retired_keys: RwLock<HashMap<String, Vec<RetiredKey>>>,
    /// Grace period in seconds for old keys after rotation (default: 24h).
    grace_period_secs: u64,
    /// Counter for generating key IDs.
    key_id_counter: std::sync::atomic::AtomicU64,
}

impl Encryptor {
    /// Create Encryptor from config. Falls back to local key provider.
    pub fn from_config(config: &EncryptionConfig, data_dir: &Path) -> anyhow::Result<Self> {
        let key_provider: Box<dyn KeyProvider> = match config.provider.as_deref() {
            Some("aws_kms") => {
                let key_id = config
                    .key_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("encryption.key_id required for aws_kms"))?;
                let region = config.region.as_deref().unwrap_or("us-east-1");
                Box::new(AwsKmsKeyProvider::new(
                    key_id.to_string(),
                    region.to_string(),
                    data_dir.to_path_buf(),
                ))
            }
            Some("vault") => {
                let address = config.vault_address.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("encryption.vault_address required for vault")
                })?;
                let token = config
                    .vault_token
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("encryption.vault_token required for vault"))?;
                let mount = config.vault_mount.as_deref().unwrap_or("transit");
                let key_name = config.vault_key_name.as_deref().unwrap_or("memoryoss");
                Box::new(VaultKeyProvider::new(
                    address.to_string(),
                    token.to_string(),
                    mount.to_string(),
                    key_name.to_string(),
                    data_dir.to_path_buf(),
                ))
            }
            _ => {
                // Default: local file-based key
                Box::new(LocalKeyProvider::load_or_create(data_dir)?)
            }
        };

        Ok(Self {
            key_provider,
            cipher_cache: RwLock::new(HashMap::new()),
            retired_keys: RwLock::new(HashMap::new()),
            grace_period_secs: config.grace_period_secs.unwrap_or(86400),
            key_id_counter: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// Backward-compatible: create with local key provider.
    pub fn load_or_create(data_dir: &Path) -> anyhow::Result<Self> {
        Self::from_config(&EncryptionConfig::default(), data_dir)
    }

    fn get_cipher(&self, namespace: &str) -> anyhow::Result<Aes256Gcm> {
        // Check cache first
        if let Ok(cache) = self.cipher_cache.read()
            && let Some(cipher) = cache.get(namespace)
        {
            return Ok(cipher.clone());
        }

        // Derive key and cache cipher
        let key_bytes = self.key_provider.get_data_key(namespace)?;
        let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(key);

        if let Ok(mut cache) = self.cipher_cache.write() {
            cache.insert(namespace.to_string(), cipher.clone());
        }

        Ok(cipher)
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.encrypt_ns(plaintext, "default")
    }

    pub fn decrypt(&self, data: &[u8]) -> anyhow::Result<Vec<u8>> {
        self.decrypt_ns(data, "default")
    }

    pub fn encrypt_ns(&self, plaintext: &[u8], namespace: &str) -> anyhow::Result<Vec<u8>> {
        let cipher = self.get_cipher(namespace)?;
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;

        let mut result = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    pub fn decrypt_ns(&self, data: &[u8], namespace: &str) -> anyhow::Result<Vec<u8>> {
        if data.len() < NONCE_SIZE {
            anyhow::bail!("ciphertext too short");
        }
        let (nonce_bytes, ciphertext) = data.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        // Try current key first
        let cipher = self.get_cipher(namespace)?;
        if let Ok(plaintext) = cipher.decrypt(nonce, ciphertext) {
            return Ok(plaintext);
        }

        // Try retired keys within grace period
        let now = chrono::Utc::now();
        if let Ok(retired) = self.retired_keys.read()
            && let Some(keys) = retired.get(namespace)
        {
            for rk in keys.iter().rev() {
                if now > rk.expires_at {
                    continue; // Expired, skip
                }
                if let Ok(plaintext) = rk.cipher.decrypt(nonce, ciphertext) {
                    return Ok(plaintext);
                }
            }
        }

        anyhow::bail!("decryption failed: no matching key (current or retired)")
    }

    /// Rotate the encryption key for a namespace.
    /// The old key is kept for `grace_period_secs` for decryption.
    /// The new key is persisted to disk before being used.
    /// Returns the new key ID.
    pub fn rotate_namespace(&self, namespace: &str) -> anyhow::Result<String> {
        // Get current cipher before rotation
        let old_cipher = self.get_cipher(namespace)?;

        // Generate new key
        let mut new_key_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut new_key_bytes);

        // Persist the rotated key BEFORE installing it in cache.
        // This ensures we never use an in-memory-only key that would be lost on restart.
        self.key_provider
            .store_rotated_key(namespace, &new_key_bytes)?;

        let new_key = Key::<Aes256Gcm>::from_slice(&new_key_bytes);
        let _new_cipher = Aes256Gcm::new(new_key);

        // Zeroize the raw key bytes now that cipher is created
        new_key_bytes.zeroize();

        // Retire old cipher
        let key_id = format!(
            "key-{}-{}",
            namespace,
            self.key_id_counter
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let now = chrono::Utc::now();
        let grace = chrono::Duration::seconds(self.grace_period_secs as i64);

        if let Ok(mut retired) = self.retired_keys.write() {
            let entries = retired
                .entry(namespace.to_string())
                .or_insert_with(Vec::new);
            entries.push(RetiredKey {
                id: key_id.clone(),
                cipher: old_cipher,
                retired_at: now,
                expires_at: now + grace,
            });

            // Clean up expired keys
            entries.retain(|rk| now < rk.expires_at);
        }

        // Clear cache so next access uses the persisted key
        if let Ok(mut cache) = self.cipher_cache.write() {
            cache.remove(namespace);
        }

        tracing::info!(
            namespace,
            key_id,
            grace_secs = self.grace_period_secs,
            "Key rotated and persisted"
        );
        Ok(key_id)
    }

    /// Immediately revoke a retired key by its ID, removing it from the grace period.
    pub fn revoke_key(&self, key_id: &str) -> bool {
        if let Ok(mut retired) = self.retired_keys.write() {
            for entries in retired.values_mut() {
                let before = entries.len();
                entries.retain(|rk| rk.id != key_id);
                if entries.len() < before {
                    tracing::info!(key_id, "Key immediately revoked");
                    return true;
                }
            }
        }
        false
    }

    /// List all active retired keys (within grace period).
    pub fn list_retired_keys(&self) -> Vec<serde_json::Value> {
        let now = chrono::Utc::now();
        let mut result = Vec::new();
        if let Ok(retired) = self.retired_keys.read() {
            for (ns, entries) in retired.iter() {
                for rk in entries {
                    if now < rk.expires_at {
                        result.push(serde_json::json!({
                            "id": rk.id,
                            "namespace": ns,
                            "retired_at": rk.retired_at.to_rfc3339(),
                            "expires_at": rk.expires_at.to_rfc3339(),
                        }));
                    }
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{EncryptionConfig, Encryptor, base64_encode};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    const TEST_TOKEN: &str = "vault-token";
    #[derive(Clone, Copy)]
    enum VaultMockMode {
        Success,
        FailDatakey,
    }

    fn handle_vault_stream(
        mut stream: TcpStream,
        mode: VaultMockMode,
        requests: &Arc<Mutex<Vec<String>>>,
    ) {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut header_len = None;
        let mut content_length = 0usize;
        loop {
            let read = stream.read(&mut chunk).expect("read request");
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if header_len.is_none()
                && let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n")
            {
                let end = pos + 4;
                header_len = Some(end);
                let headers = String::from_utf8_lossy(&buffer[..end]);
                for line in headers.lines() {
                    if let Some(value) = line.strip_prefix("Content-Length:") {
                        content_length = value.trim().parse::<usize>().expect("content length");
                    }
                }
            }
            if let Some(end) = header_len
                && buffer.len() >= end + content_length
            {
                break;
            }
        }

        let request = String::from_utf8_lossy(&buffer).to_string();
        requests.lock().unwrap().push(request.clone());
        let first_line = request.lines().next().unwrap_or_default().to_string();
        let seed_b64 = base64_encode(&(0u8..32).collect::<Vec<u8>>());
        let rotated_b64 = base64_encode(&(32u8..64).collect::<Vec<u8>>());

        let (status, body) = if matches!(mode, VaultMockMode::FailDatakey)
            && first_line.contains("/datakey/plaintext/")
        {
            (
                "500 Internal Server Error",
                r#"{"errors":["boom"]}"#.to_string(),
            )
        } else if first_line.contains("/datakey/plaintext/") {
            (
                "200 OK",
                format!(r#"{{"data":{{"plaintext":"{seed_b64}","ciphertext":"vault:v1:seed"}}}}"#),
            )
        } else if first_line.contains("/decrypt/") {
            let plaintext_b64 = if request.contains("vault:v1:rotated") {
                rotated_b64
            } else {
                seed_b64
            };
            (
                "200 OK",
                format!(r#"{{"data":{{"plaintext":"{plaintext_b64}"}}}}"#),
            )
        } else if first_line.contains("/encrypt/") {
            assert!(
                request.contains(TEST_TOKEN),
                "Vault request must carry token"
            );
            assert!(
                request.contains("\"plaintext\":\""),
                "Vault encrypt request must carry plaintext"
            );
            (
                "200 OK",
                r#"{"data":{"ciphertext":"vault:v1:rotated"}}"#.to_string(),
            )
        } else {
            ("404 Not Found", r#"{"errors":["not found"]}"#.to_string())
        };

        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    }

    fn spawn_vault_mock(
        expected_calls: usize,
        mode: VaultMockMode,
    ) -> (String, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind vault mock");
        let address = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_clone = Arc::clone(&requests);
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(expected_calls) {
                handle_vault_stream(stream.expect("vault stream"), mode, &requests_clone);
            }
        });
        (address, requests, handle)
    }

    #[test]
    fn vault_provider_roundtrip_rotation_and_reload_are_hermetic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (address, requests, handle) = spawn_vault_mock(4, VaultMockMode::Success);
        let config = EncryptionConfig {
            provider: Some("vault".to_string()),
            vault_address: Some(address),
            vault_token: Some(TEST_TOKEN.to_string()),
            vault_mount: Some("transit".to_string()),
            vault_key_name: Some("memoryoss".to_string()),
            ..EncryptionConfig::default()
        };

        let encryptor = Encryptor::from_config(&config, tmp.path()).expect("vault encryptor");
        let encrypted = encryptor
            .encrypt_ns(b"hello vault", "team")
            .expect("vault encrypt");
        assert_eq!(
            encryptor
                .decrypt_ns(&encrypted, "team")
                .expect("vault decrypt"),
            b"hello vault"
        );

        let reloaded = Encryptor::from_config(&config, tmp.path()).expect("reloaded encryptor");
        assert_eq!(
            reloaded
                .decrypt_ns(&encrypted, "team")
                .expect("reload decrypt"),
            b"hello vault"
        );

        let old_ciphertext = encrypted.clone();
        let key_id = encryptor.rotate_namespace("team").expect("vault rotate");
        assert!(key_id.starts_with("key-team-"));
        let rotated = encryptor
            .encrypt_ns(b"hello rotated", "team")
            .expect("encrypt after rotate");
        assert_eq!(
            encryptor
                .decrypt_ns(&old_ciphertext, "team")
                .expect("old ciphertext still readable"),
            b"hello vault"
        );
        assert_eq!(
            encryptor
                .decrypt_ns(&rotated, "team")
                .expect("rotated ciphertext readable"),
            b"hello rotated"
        );

        handle.join().expect("vault mock join");
        let request_dump = requests.lock().unwrap().join("\n---\n");
        assert!(request_dump.contains("/v1/transit/datakey/plaintext/memoryoss"));
        assert!(request_dump.contains("/v1/transit/decrypt/memoryoss"));
        assert!(request_dump.contains("/v1/transit/encrypt/memoryoss"));
    }

    #[test]
    fn vault_provider_fails_closed_on_bootstrap_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (address, _requests, handle) = spawn_vault_mock(1, VaultMockMode::FailDatakey);
        let config = EncryptionConfig {
            provider: Some("vault".to_string()),
            vault_address: Some(address),
            vault_token: Some(TEST_TOKEN.to_string()),
            vault_mount: Some("transit".to_string()),
            vault_key_name: Some("memoryoss".to_string()),
            ..EncryptionConfig::default()
        };

        let encryptor = Encryptor::from_config(&config, tmp.path()).expect("vault encryptor");
        let err = encryptor
            .encrypt_ns(b"will fail", "team")
            .expect_err("vault bootstrap should fail");
        assert!(err.to_string().contains("Vault datakey generation failed"));
        handle.join().expect("vault mock join");
    }

    #[test]
    fn aws_kms_provider_fails_closed_and_is_not_treated_as_supported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = EncryptionConfig {
            provider: Some("aws_kms".to_string()),
            key_id: Some("alias/memoryoss".to_string()),
            region: Some("eu-central-1".to_string()),
            ..EncryptionConfig::default()
        };

        let encryptor = Encryptor::from_config(&config, tmp.path()).expect("aws kms config");
        let err = encryptor
            .encrypt_ns(b"hello", "team")
            .expect_err("aws kms should fail closed");
        assert!(
            err.to_string()
                .contains("AWS KMS key provider is not yet implemented")
        );
    }
}
