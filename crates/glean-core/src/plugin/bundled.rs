//! 官方内置插件：随应用启动时自动同步到插件目录。
//!
//! 插件以独立目录分发（仓库 `plugins/<id>/`），安装包不包含插件。为了让
//! 新构建的适配器修复（如 civitai 封面帧）能自动生效，这里把官方插件
//! 内嵌进程序，启动时对比版本：缺失或版本落后则安装/更新，否则不动
//! （保留用户对已安装插件的修改）。

use crate::error::{CoreError, Result};
use std::path::PathBuf;

/// 一个内置插件：id + manifest 版本 + 文件列表（相对路径 → 内容）。
pub struct BundledPlugin {
    pub id: &'static str,
    pub version: &'static str,
    pub files: &'static [(&'static str, &'static str)],
}

/// 官方内置插件清单。版本号必须与对应 `plugins/<id>/manifest.toml` 一致。
pub const BUNDLED_PLUGINS: &[BundledPlugin] = &[
    BundledPlugin {
        id: "bilibili",
        version: "0.1.0",
        files: &[
            (
                "manifest.toml",
                include_str!("../../../../plugins/bilibili/manifest.toml"),
            ),
            (
                "adapter.rhai",
                include_str!("../../../../plugins/bilibili/adapter.rhai"),
            ),
        ],
    },
    BundledPlugin {
        id: "civitai",
        version: "0.3.3",
        files: &[
            (
                "manifest.toml",
                include_str!("../../../../plugins/civitai/manifest.toml"),
            ),
            (
                "adapter.rhai",
                include_str!("../../../../plugins/civitai/adapter.rhai"),
            ),
        ],
    },
    BundledPlugin {
        id: "fanbox",
        version: "0.1.0",
        files: &[
            (
                "manifest.toml",
                include_str!("../../../../plugins/fanbox/manifest.toml"),
            ),
            (
                "adapter.rhai",
                include_str!("../../../../plugins/fanbox/adapter.rhai"),
            ),
        ],
    },
    BundledPlugin {
        id: "fantia",
        version: "0.1.0",
        files: &[
            (
                "manifest.toml",
                include_str!("../../../../plugins/fantia/manifest.toml"),
            ),
            (
                "adapter.rhai",
                include_str!("../../../../plugins/fantia/adapter.rhai"),
            ),
        ],
    },
    BundledPlugin {
        id: "pixiv",
        version: "0.1.1",
        files: &[
            (
                "manifest.toml",
                include_str!("../../../../plugins/pixiv/manifest.toml"),
            ),
            (
                "adapter.rhai",
                include_str!("../../../../plugins/pixiv/adapter.rhai"),
            ),
        ],
    },
];

/// 把内置插件同步到 `plugins_dir`：缺失或版本落后时安装/更新。
/// 返回本次安装/更新的插件 id 列表。失败不阻塞（插件是扩展层）。
pub fn sync_bundled_plugins(plugins_dir: &PathBuf) -> Vec<String> {
    let mut synced = Vec::new();
    for plugin in BUNDLED_PLUGINS {
        let target = plugins_dir.join(plugin.id);
        let installed = installed_version(&target);
        let needs_update = match &installed {
            None => true,
            Some(v) => version_gt(plugin.version, v),
        };
        if !needs_update {
            continue;
        }
        match write_plugin_atomic(&target, plugin) {
            Ok(()) => synced.push(plugin.id.to_string()),
            Err(e) => eprintln!("glean: 内置插件 {} 同步失败: {e}", plugin.id),
        }
    }
    synced
}

/// 读取已安装插件的 manifest 版本；目录不存在或 manifest 损坏时返回 None。
fn installed_version(target: &PathBuf) -> Option<String> {
    let text = std::fs::read_to_string(target.join("manifest.toml")).ok()?;
    let manifest: crate::plugin::manifest::Manifest = toml::from_str(&text).ok()?;
    Some(manifest.plugin.version)
}

/// 简单版本比较（按 `.` 分段数值比较）。`a > b` 返回 true。
fn version_gt(a: &str, b: &str) -> bool {
    let pa: Vec<u64> = a.split('.').filter_map(|s| s.parse().ok()).collect();
    let pb: Vec<u64> = b.split('.').filter_map(|s| s.parse().ok()).collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// 原子写入插件目录：先写 staging 再 rename，失败时旧插件不受影响。
fn write_plugin_atomic(target: &PathBuf, plugin: &BundledPlugin) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| CoreError::Message("插件目录无父目录".into()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| CoreError::Message(format!("创建插件目录失败: {e}")))?;
    let staging = parent.join(format!(
        ".staging-{}-{}",
        plugin.id,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| CoreError::Message(format!("创建 staging 失败: {e}")))?;
    for (name, content) in plugin.files {
        std::fs::write(staging.join(name), content)
            .map_err(|e| CoreError::Message(format!("写入 {} 失败: {e}", plugin.id)))?;
    }
    if target.exists() {
        std::fs::remove_dir_all(target)
            .map_err(|e| CoreError::Message(format!("移除旧插件目录失败: {e}")))?;
    }
    std::fs::rename(&staging, target)
        .map_err(|e| CoreError::Message(format!("替换插件目录失败: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_gt_compares_numerically() {
        assert!(version_gt("0.2.0", "0.1.0"));
        assert!(version_gt("1.0.0", "0.9.9"));
        assert!(!version_gt("0.1.0", "0.1.0"));
        assert!(!version_gt("0.1.0", "0.2.0"));
    }

    #[test]
    fn sync_installs_missing_and_updates_outdated() {
        let tmp = std::env::temp_dir().join(format!("glean-bundled-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let plugins_dir = tmp.join("plugins");
        std::fs::create_dir_all(&plugins_dir).unwrap();

        // 首次同步：全部安装。
        let synced = sync_bundled_plugins(&plugins_dir);
        assert_eq!(synced.len(), BUNDLED_PLUGINS.len());
        for p in BUNDLED_PLUGINS {
            assert!(plugins_dir.join(p.id).join("manifest.toml").is_file());
            assert!(plugins_dir.join(p.id).join("adapter.rhai").is_file());
        }

        // 再次同步：版本一致，无更新。
        let synced2 = sync_bundled_plugins(&plugins_dir);
        assert!(synced2.is_empty());

        // 模拟旧版本：把 civitai 版本改成 0.1.0，应触发更新。
        let current = BUNDLED_PLUGINS
            .iter()
            .find(|p| p.id == "civitai")
            .expect("civitai bundled")
            .version;
        let civitai_dir = plugins_dir.join("civitai");
        let manifest_path = civitai_dir.join("manifest.toml");
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        let from = format!("version = \"{current}\"");
        assert!(
            text.contains(&from),
            "civitai manifest 应含当前内置版本 {current}: {text}"
        );
        let updated = text.replace(&from, "version = \"0.1.0\"");
        std::fs::write(&manifest_path, updated).unwrap();
        let synced3 = sync_bundled_plugins(&plugins_dir);
        assert_eq!(synced3, vec!["civitai"]);
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(
            text.contains(&from),
            "更新后应恢复到内置版本 {current}: {text}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
