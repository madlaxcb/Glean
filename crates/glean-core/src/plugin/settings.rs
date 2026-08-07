//! 插件自定义设置存储。
//!
//! 与凭证存储不同，设置值是非敏感的明文配置（如域名选择），
//! 不需要加密。按 plugin_id + key 索引，持久化为 JSON 文件。

use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// 插件设置存储：按 plugin_id → key → value 索引。
#[derive(Clone)]
pub struct PluginSettings {
    path: PathBuf,
    entries: HashMap<String, HashMap<String, String>>,
    dirty: bool,
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    plugins: HashMap<String, HashMap<String, String>>,
}

impl PluginSettings {
    /// 打开/创建 `<data_dir>/plugin_settings.json`。
    pub fn open(path: PathBuf) -> Result<Self> {
        let mut store = Self {
            path,
            entries: HashMap::new(),
            dirty: false,
        };
        if let Ok(text) = std::fs::read_to_string(&store.path) {
            if let Ok(data) = serde_json::from_str::<FileFormat>(&text) {
                store.entries = data.plugins;
            }
        }
        Ok(store)
    }

    /// 内存模式（测试用）。
    pub fn in_memory() -> Self {
        Self {
            path: PathBuf::new(),
            entries: HashMap::new(),
            dirty: false,
        }
    }

    /// 读取插件设置值。未设置时返回 manifest 声明的默认值。
    pub fn get(&self, plugin_id: &str, key: &str) -> Option<&str> {
        self.entries
            .get(plugin_id)
            .and_then(|m| m.get(key))
            .map(|v| v.as_str())
    }

    /// 读取插件全部设置值。
    pub fn get_all(&self, plugin_id: &str) -> HashMap<String, String> {
        self.entries.get(plugin_id).cloned().unwrap_or_default()
    }

    /// 设置插件配置项。
    pub fn set(&mut self, plugin_id: &str, key: &str, value: &str) {
        self.entries
            .entry(plugin_id.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
        self.dirty = true;
    }

    /// 删除插件配置项。
    pub fn remove(&mut self, plugin_id: &str, key: &str) {
        if let Some(m) = self.entries.get_mut(plugin_id) {
            m.remove(key);
            self.dirty = true;
        }
    }

    /// 落盘。
    pub fn flush(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if self.path.as_os_str().is_empty() {
            self.dirty = false;
            return Ok(());
        }
        let data = FileFormat {
            plugins: self.entries.clone(),
        };
        let text = serde_json::to_string_pretty(&data)
            .map_err(|e| CoreError::Message(format!("序列化设置失败: {e}")))?;
        std::fs::write(&self.path, text)
            .map_err(|e| CoreError::Message(format!("写入设置文件失败: {e}")))?;
        self.dirty = false;
        Ok(())
    }
}
