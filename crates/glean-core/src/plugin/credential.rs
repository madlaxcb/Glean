//! 凭证存储。§11.5.9
//!
//! **核心原则（§11.5.4）：插件永远拿不到明文凭证。**
//! 用户在设置里粘贴 Cookie/API Key → Glean 加密落盘 →
//! Host 在 `http_get` 内部注入 Header，Rhai 脚本只声明"请附上 pixiv_session"。
//!
//! M5 状态（§11.5.11 路线图）：
//! - 抽象 + 内存存储 + JSON 落盘已就绪
//! - Windows DPAPI (`CryptProtectData`) 与 Linux `keyring` 接入排到 M6
//!   —— 当前 `scheme = "plaintext-stub"`，仅开发期占位，**生产部署前必须替换**
//!
//! 设计上 `CredentialStore` 已把"加密方案"独立成 `EncryptedBlob.scheme`，
//! M6 增加 `dpapi` / `keyring` 分支时无需改动调用方。

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
/// 当前唯一方案是 `plaintext-stub`（开发期占位）。
/// M6 增加 `dpapi` (Windows) / `keyring` (Linux) 分支时，
/// 调用方接口不变，只在本类型上加分支。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedBlob {
    /// `plaintext-stub` | `dpapi` (M6) | `keyring` (M6)
    scheme: String,
    /// base64 编码的密文（dpapi）或明文 UTF-8（stub）。
    data: String,
}

fn encrypt(plaintext: &str) -> Result<EncryptedBlob> {
    // M5 开发期占位：明文存储。M6 替换为 DPAPI / keyring。
    Ok(EncryptedBlob {
        scheme: "plaintext-stub".into(),
        data: plaintext.to_string(),
    })
}

fn decrypt(blob: &EncryptedBlob) -> Result<String> {
    if blob.scheme != "plaintext-stub" {
        return Err(CoreError::Message(format!(
            "unsupported credential scheme: {} (M6 will add dpapi/keyring)",
            blob.scheme
        )));
    }
    Ok(blob.data.clone())
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
    fn roundtrip_plaintext_stub() {
        let blob = encrypt("hello world").unwrap();
        assert_eq!(blob.scheme, "plaintext-stub");
        let back = decrypt(&blob).unwrap();
        assert_eq!(back, "hello world");
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
