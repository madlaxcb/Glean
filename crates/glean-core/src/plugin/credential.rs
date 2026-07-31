//! 凭证存储。§11.5.9
//!
//! **核心原则（§11.5.4）：插件永远拿不到明文凭证。**
//! 用户在设置里粘贴 Cookie/API Key → Glean 加密落盘 →
//! Host 在 `http_get` 内部注入 Header，Rhai 脚本只声明"请附上 pixiv_session"。
//!
//! 加密方案（`EncryptedBlob.scheme`）：
//! - `dpapi` (Windows): `CryptProtectData` / `CryptUnprotectData`，密文 base64
//! - `plaintext-stub`: 非 Windows 开发期占位 + 旧版文件兼容读
//!
//! 项目目标平台为 Windows；非 Windows 分支仅为开发环境能编译跑测试而保留。
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
    /// `plaintext-stub` | `dpapi`
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
        // 非 Windows 开发期占位：明文存储，仅用于让开发环境能编译跑测试。
        Ok(EncryptedBlob {
            scheme: "plaintext-stub".into(),
            data: plaintext.to_string(),
        })
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
        other => Err(CoreError::Message(format!(
            "unsupported credential scheme: {other}"
        ))),
    }
}

/// 加密任意敏感字符串（如 AI api_key），返回 JSON 序列化的 `EncryptedBlob`。
///
/// 复用 `encrypt`/`decrypt` 原语，与插件凭证同一套加密路径（Windows DPAPI，
/// Linux 开发期 plaintext-stub）。AppConfig 把返回字符串作为不透明字段存储，
/// 用 [`decrypt_secret`] 还原明文。与 `CredentialStore` 结构上隔离但加密同源。
///
/// `pub`：设置 UI（glean-app）在保存 AI 配置时加密用户输入的 api_key。
pub fn encrypt_secret(plaintext: &str) -> Result<String> {
    let blob = encrypt(plaintext)?;
    serde_json::to_string(&blob).map_err(|e| CoreError::Message(format!("secret serialize: {e}")))
}

/// 还原 [`encrypt_secret`] 的输出为明文。空字符串输入返回空字符串
/// （未配置 api_key 的常见情况，避免无谓报错）。
pub fn decrypt_secret(blob_json: &str) -> Result<String> {
    if blob_json.is_empty() {
        return Ok(String::new());
    }
    let blob: EncryptedBlob = serde_json::from_str(blob_json)
        .map_err(|e| CoreError::Message(format!("secret parse: {e}")))?;
    decrypt(&blob)
}

// DPAPI 输出 buffer 必须用 `LocalFree` 释放（Win32 规范）。
// windows crate 0.58 移除了 `LocalFree` 符号，这里直接 FFI 声明
// （kernel32.dll 永远导出，签名固定）。
#[cfg(windows)]
extern "system" {
    fn LocalFree(hmem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

#[cfg(windows)]
fn encrypt_dpapi(plaintext: &str) -> Result<EncryptedBlob> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let bytes = plaintext.as_bytes();
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
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
        let _ = LocalFree(out_blob.pbData as *mut std::ffi::c_void);
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
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let cipher = STANDARD
        .decode(data_b64)
        .map_err(|e| CoreError::Message(format!("dpapi base64 decode: {e}")))?;
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: cipher.len() as u32,
        pbData: cipher.as_ptr() as *mut u8,
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
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
        let _ = LocalFree(out_blob.pbData as *mut std::ffi::c_void);
        String::from_utf8(plain)
            .map_err(|e| CoreError::Message(format!("dpapi plaintext utf8: {e}")))
    }
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
        // Windows 走 dpapi；非 Windows 走 plaintext-stub 占位。任一路径都要 roundtrip。
        let blob = encrypt("hello world").unwrap();
        let back = decrypt(&blob).unwrap();
        assert_eq!(back, "hello world");
    }

    #[test]
    fn secret_roundtrip_via_json() {
        // AppConfig 存储路径：encrypt_secret → JSON 字符串 → decrypt_secret 还原。
        let cipher = encrypt_secret("sk-test-12345").expect("encrypt");
        // 加密输出不应包含明文（dpapi 是二进制 base64；plaintext-stub 在 Linux 开发期会含明文，那是预期的）。
        #[cfg(windows)]
        assert!(!cipher.contains("sk-test-12345"));
        let back = decrypt_secret(&cipher).expect("decrypt");
        assert_eq!(back, "sk-test-12345");
    }

    #[test]
    fn decrypt_secret_empty_returns_empty() {
        // 未配置 api_key 时 cipher 为空字符串，不应报错。
        assert_eq!(decrypt_secret("").unwrap(), "");
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

    #[test]
    fn load_missing_file_is_empty() {
        let tmp = std::env::temp_dir().join(format!("glean-cred-test-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let store = CredentialStore::open(tmp.clone()).expect("open");
        assert!(store.entries_ref().is_empty());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn flush_then_reopen_persists() {
        // 落盘后重新 open，凭证仍在（Windows dpapi / Linux stub 都要通过）。
        let tmp = std::env::temp_dir().join(format!(
            "glean-cred-roundtrip-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&tmp);
        {
            let mut store = CredentialStore::open(tmp.clone()).expect("open");
            store.set(
                "pixiv",
                "pixiv_session",
                Credential {
                    header_name: "Cookie".into(),
                    header_value: "PHPSESSID=abc".into(),
                },
            );
            store.flush().expect("flush");
        }
        let store = CredentialStore::open(tmp.clone()).expect("reopen");
        let cred = store.get("pixiv", "pixiv_session").expect("persisted");
        assert_eq!(cred.header_name, "Cookie");
        assert_eq!(cred.header_value, "PHPSESSID=abc");
        let _ = std::fs::remove_file(&tmp);
    }

    impl CredentialStore {
        fn entries_ref(&self) -> &HashMap<String, Credential> {
            &self.entries
        }
    }
}
