//! 插件系统。§11.5
//!
//! 模块布局：
//! - [`manifest`]：manifest.toml serde 结构（§11.5.8）
//! - [`credential`]：凭证存储（§11.5.9，DPAPI / keyring）
//! - [`runtime`]：Rhai Engine 构建 + host 函数注册（§11.5.4 / §11.5.6）
//! - [`tier1`]：Tier 1 配置驱动适配器（§11.5.2）
//! - [`manager`]：PluginManager，扫描插件目录 + URL 路由
//!
//! 插件以独立目录分发（仓库 `plugins/<id>/`），程序不内嵌任何插件；安装 /
//! 卸载 / 启停由「插件管理」界面完成。
//!
//! 核心安全决策（§11.5.4 / §11.5.12）：
//! 1. **能力原语 + 作用域**：manifest 声明能力，Host 在执行时强制校验范围
//! 2. **凭证零接触**：插件永远拿不到明文凭证，Host 在 `http_get` 内部注入 Header
//! 3. **输出回消毒管线**：插件产出的 HTML 在入库前必过 ammonia
//! 4. **最小权限**：`Engine` 按插件动态构建，只注册声明过的 host 函数
//! 5. **不阻塞主循环**：所有 hook 在 worker 线程，超时由 Host 强制

pub mod bundled;
pub mod credential;
pub mod enhancer;
pub mod manager;
pub mod manifest;
pub mod runtime;
pub mod settings;
pub mod tier1;

pub use credential::{Credential, CredentialStore};
pub use enhancer::{Enhancer, EntryPatch, HostApi};
pub use manager::{InstallPreview, LoadedPlugin, PluginManager};
pub use manifest::{
    Capabilities, Compliance, Manifest, MatchRule, PluginMeta, SettingField, Tier, Tier1Config,
    Tier1FieldMap,
};
pub use runtime::Runtime;
pub use settings::PluginSettings;
