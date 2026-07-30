//! 凭证存储。§11.5.9
//!
//! **核心原则（§11.5.4）：插件永远拿不到明文凭证。**
//! 用户在设置里粘贴 Cookie/API Key → Glean 加密落盘 →
//! Host 在 `http_get` 内部注入 Header，Rhai 脚本只声明"请附上 pixiv_session"。
//!
//! 加密方案（`EncryptedBlob.scheme`）：
//! - `dpapi` (Windows): `CryptProtectData` / `CryptUnprotectData`，密文 base64
//! - `keyring-aes-gcm` (Linux): master key 存 `keyring` (secret-service)，
//!   AES-256-GCM 加密 blob，`data = base64(nonce || ct)`
//! - `plaintext-stub`: 旧版兼容读 + Linux 无 secret-service 时的 fallback（带 warn）
//!
//! 调用方接口（`CredentialStore::open/flush/get/set/remove`）不随 scheme 变化。

use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[cfg(windows)]
const IS_WINDOWS: bool = true;
#[cfg(not(windows))]
const IS_WINDOWS: bool = false;

/// 凭证值：HTTP header 名 + 值（Host 注入用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    /// 形如 `Authorization` / `Cookie`。
    pub header_name: String,
    /// 明文值（落盘前会被加密）。
    pub header_value: String,
}

/// 凭证存储：按 plugin_id + slot 索引。
///
/// `Clone` 用于刷新时给 worker 线程一份快照（凭证集很小，克隆成本可忽略）。
/// 主副本留在 `GleanService` 中负责可变写入 + 落盘。
#[derive(Clone)]
pub struct CredentialStore {
    path: PathBuf,
    /// 内存中的明文缓存；落盘时加密。
    entries: HashMap<String, Credential>,
    dirty: bool,
}

impl CredentialStore {
    /// 打开/创建 `<data_dir>/credentials.json`。
    pub fn open(path: PathBuf) -> Result<Self> {
        let mut store = Self {
            path,
            entries: HashMap::new(),
            dirty: false,
        };
        store.load()?;
        Ok(store)
    }

    /// 内联构造（测试用）。
    pub fn in_memory() -> Self {
        Self {
            path: PathBuf::new(),
            entries: HashMap::new(),
            dirty: false,
        }
    }

    pub fn is_windows() -> bool {
        IS_WINDOWS
    }

    /// 取凭证明文。Host 在 `http_get` 内部调用，永远不传给 Rhai 脚本。
    pub fn get(&self, plugin_id: &str, slot: &str) -> Option<&Credential> {
        self.entries.get(&key(plugin_id, slot))
    }

    /// 设置凭证（用户在设置 UI 输入后调用）。
    pub fn set(&mut self, plugin_id: &str, slot: &str, cred: Credential) {
        self.entries.insert(key(plugin_id, slot), cred);
        self.dirty = true;
    }

    pub fn remove(&mut self, plugin_id: &str, slot: &str) -> bool {
        let removed = self.entries.remove(&key(plugin_id, slot)).is_some();
        if removed {
            self.dirty = true;
        }
        removed
    }

    fn load(&mut self) -> Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let Ok(bytes) = std::fs::read(&self.path) else {
            return Ok(()); // 文件不存在视为空存储。
        };
        if bytes.is_empty() {
            return Ok(());
        }
        let blob: EncryptedBlob = serde_json::from_slice(&bytes)
            .map_err(|e| CoreError::Message(format!("credentials parse: {e}")))?;
        let plaintext = decrypt(&blob)?;
        self.entries = serde_json::from_str(&plaintext)
            .map_err(|e| CoreError::Message(format!("credentials json: {e}")))?;
        Ok(())
    }

    /// 显式落盘。`set` 之后必须调用 `flush` 才会持久化。
    pub fn flush(&mut self) -> Result<()> {
        if !self.dirty || self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let plaintext = serde_json::to_string(&self.entries)
            .map_err(|e| CoreError::Message(format!("credentials serialize: {e}")))?;
        let blob = encrypt(&plaintext)?;
        let bytes = serde_json::to_vec_pretty(&blob)
            .map_err(|e| CoreError::Message(format!("credentials blob serialize: {e}")))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&self.path, bytes)
            .map_err(|e| CoreError::Message(format!("credentials write: {e}")))?;
        self.dirty = false;
        Ok(())
    }
}

fn key(plugin_id: &str, slot: &str) -> String {
    format!("{plugin_id}:{slot}")
}

/// 落盘形态：加密后的密文 + 使用的加密方案标识。
///
/// `scheme` 取值见模块顶部文档。`decrypt` 按字段分发，因此旧文件
/// （`plaintext-stub`）在新代码上仍可读 —— 升级时下次 `flush` 会以
/// 当前平台 scheme 重写。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedBlob {
    /// `plaintext-stub` | `dpapi` | `keyring-aes-gcm`
    scheme: String,
    /// base64 编码的密文；`plaintext-stub` 时为明文 UTF-8。
    data: String,
}

fn encrypt(plaintext: &str) -> Result<EncryptedBlob> {
    #[cfg(windows)]
    {
        encrypt_dpapi(plaintext)
    }
    #[cfg(not(windows))]
    {
        match encrypt_keyring(plaintext) {
            Ok(blob) => Ok(blob),
            Err(e) => {
                eprintln!(
                    "warning: Glean keyring encryption unavailable ({}); falling back to \
                     plaintext-stub. Install a secret-service daemon (e.g. gnome-keyring) \
                     to enable secure storage.",
                    e
                );
                Ok(EncryptedBlob {
                    scheme: "plaintext-stub".into(),
                    data: plaintext.to_string(),
                })
            }
        }
    }
}

fn decrypt(blob: &EncryptedBlob) -> Result<String> {
    match blob.scheme.as_str() {
        "plaintext-stub" => Ok(blob.data.clone()),
        "dpapi" => {
            #[cfg(windows)]
            {
                decrypt_dpapi(&blob.data)
            }
            #[cfg(not(windows))]
            {
                Err(CoreError::Message(
                    "credential scheme 'dpapi' requires Windows".into(),
                ))
            }
        }
        "keyring-aes-gcm" => {
            #[cfg(windows)]
            {
                Err(CoreError::Message(
                    "credential scheme 'keyring-aes-gcm' requires Linux".into(),
                ))
            }
            #[cfg(not(windows))]
            {
                decrypt_keyring(&blob.data)
            }
        }
        other => Err(CoreError::Message(format!(
            "unsupported credential scheme: {other}"
        ))),
    }
}

#[cfg(windows)]
fn encrypt_dpapi(plaintext: &str) -> Result<EncryptedBlob> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, DATA_BLOB,
    };
    use windows::Win32::System::Memory::LocalFree;

    let bytes = plaintext.as_bytes();
    let mut in_blob = DATA_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut out_blob = DATA_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        CryptProtectData(
            &in_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
        .map_err(|e| CoreError::Message(format!("CryptProtectData: {e}")))?;
        let cipher = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize);
        let b64 = STANDARD.encode(cipher);
        LocalFree(out_blob.pbData as *mut _).ok();
        Ok(EncryptedBlob {
            scheme: "dpapi".into(),
            data: b64,
        })
    }
}

#[cfg(windows)]
fn decrypt_dpapi(data_b64: &str) -> Result<String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, DATA_BLOB,
    };
    use windows::Win32::System::Memory::LocalFree;

    let cipher = STANDARD
        .decode(data_b64)
        .map_err(|e| CoreError::Message(format!("dpapi base64 decode: {e}")))?;
    let mut in_blob = DATA_BLOB {
        cbData: cipher.len() as u32,
        pbData: cipher.as_ptr() as *mut u8,
    };
    let mut out_blob = DATA_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        CryptUnprotectData(
            &in_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
        .map_err(|e| CoreError::Message(format!("CryptUnprotectData: {e}")))?;
        let plain = std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec();
        LocalFree(out_blob.pbData as *mut _).ok();
        String::from_utf8(plain)
            .map_err(|e| CoreError::Message(format!("dpapi plaintext utf8: {e}")))
    }
}

#[cfg(not(windows))]
fn encrypt_keyring(plaintext: &str) -> Result<EncryptedBlob> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let key = get_or_create_master_key()?;
    let ct = aes_gcm_encrypt(&key, plaintext.as_bytes())?;
    let b64 = STANDARD.encode(ct);
    Ok(EncryptedBlob {
        scheme: "keyring-aes-gcm".into(),
        data: b64,
    })
}

#[cfg(not(windows))]
fn decrypt_keyring(data_b64: &str) -> Result<String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let data = STANDARD
        .decode(data_b64)
        .map_err(|e| CoreError::Message(format!("keyring base64 decode: {e}")))?;
    let key = get_or_create_master_key()?;
    let plain = aes_gcm_decrypt(&key, &data)?;
    String::from_utf8(plain).map_err(|e| CoreError::Message(format!("keyring plaintext utf8: {e}")))
}

/// 从 Linux keyring 取（或首次创建）AES-256 master key。
///
/// 失败时返回 `Err`，由 `encrypt` 上层 fallback 到 `plaintext-stub`。
#[cfg(not(windows))]
fn get_or_create_master_key() -> Result<[u8; 32]> {
    use base64::Engine;
    use keyring::Entry;
    const SERVICE: &str = "Glean";
    const USER: &str = "credentials-master-key";

    let entry =
        Entry::new(SERVICE, USER).map_err(|e| CoreError::Message(format!("keyring entry: {e}")))?;
    match entry.get_password() {
        Ok(b64) => {
            let raw = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| CoreError::Message(format!("master key b64 decode: {e}")))?;
            if raw.len() != 32 {
                return Err(CoreError::Message(format!(
                    "master key length {} != 32",
                    raw.len()
                )));
            }
            let mut k = [0u8; 32];
            k.copy_from_slice(&raw);
            Ok(k)
        }
        Err(keyring::Error::NoEntry) => {
            let mut k = [0u8; 32];
            use rand::RngCore;
            rand::thread_rng().fill_bytes(&mut k);
            let b64 = base64::engine::general_purpose::STANDARD.encode(k);
            entry
                .set_password(&b64)
                .map_err(|e| CoreError::Message(format!("keyring set_password: {e}")))?;
            Ok(k)
        }
        Err(e) => Err(CoreError::Message(format!("keyring get_password: {e}"))),
    }
}

/// AES-256-GCM 加密。输出 = `nonce(12) || ct`。
#[cfg(not(windows))]
fn aes_gcm_encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};
    use rand::RngCore;

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| CoreError::Message(format!("aes-gcm key init: {e}")))?;
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct: Vec<u8> = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| CoreError::Message(format!("aes-gcm encrypt: {e}")))?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// AES-256-GCM 解密。输入 = `nonce(12) || ct`。
#[cfg(not(windows))]
fn aes_gcm_decrypt(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};

    if data.len() < 12 {
        return Err(CoreError::Message("aes-gcm data too short".into()));
    }
    let (nonce_bytes, ct) = data.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| CoreError::Message(format!("aes-gcm key init: {e}")))?;
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ct)
        .map_err(|e| CoreError::Message(format!("aes-gcm decrypt: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_set_get_remove() {
        let mut store = CredentialStore::in_memory();
        store.set(
            "pixiv",
            "pixiv_session",
            Credential {
                header_name: "Cookie".into(),
                header_value: "PHPSESSID=abc".into(),
            },
        );
        let cred = store.get("pixiv", "pixiv_session").expect("present");
        assert_eq!(cred.header_name, "Cookie");
        assert_eq!(cred.header_value, "PHPSESSID=abc");
        assert!(store.remove("pixiv", "pixiv_session"));
        assert!(store.get("pixiv", "pixiv_session").is_none());
    }

    #[test]
    fn flush_no_path_is_noop() {
        let mut store = CredentialStore::in_memory();
        store.set(
            "x",
            "y",
            Credential {
                header_name: "Authorization".into(),
                header_value: "Bearer tok".into(),
            },
        );
        store.flush().expect("noop ok");
    }

    #[test]
    fn roundtrip_default_scheme() {
        // 平台无关：Windows 走 dpapi；Linux 有 keyring 走 keyring-aes-gcm；
        // Linux 无 keyring fallback 到 plaintext-stub。任一路径都要 roundtrip。
        let blob = encrypt("hello world").unwrap();
        let back = decrypt(&blob).unwrap();
        assert_eq!(back, "hello world");
    }

    #[test]
    fn decrypt_legacy_plaintext_stub() {
        // 旧版文件 scheme=plaintext-stub，新代码必须能读（向后兼容）。
        let blob = EncryptedBlob {
            scheme: "plaintext-stub".into(),
            data: "legacy-secret".into(),
        };
        assert_eq!(decrypt(&blob).unwrap(), "legacy-secret");
    }

    #[cfg(not(windows))]
    #[test]
    #[ignore = "requires secret-service daemon (gnome-keyring/kwallet) on Linux"]
    fn roundtrip_keyring_aes_gcm_explicit() {
        // 直接调 keyring 路径，不走 fallback；本机无 secret-service 时跳过。
        let plaintext = "keyring-secret";
        let blob = encrypt_keyring(plaintext).expect("keyring encryption");
        assert_eq!(blob.scheme, "keyring-aes-gcm");
        let back = decrypt_keyring(&blob.data).expect("keyring decryption");
        assert_eq!(back, plaintext);
    }

    #[test]
    fn load_missing_file_is_empty() {
        let tmp = std::env::temp_dir().join(format!("glean-cred-test-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let store = CredentialStore::open(tmp.clone()).expect("open");
        assert!(store.entries_ref().is_empty());
        let _ = std::fs::remove_file(&tmp);
    }

    impl CredentialStore {
        fn entries_ref(&self) -> &HashMap<String, Credential> {
            &self.entries
        }
    }
}
