//! 内置默认插件。§11.5.11
//!
//! 这些插件以 `include_str!` 嵌入二进制，随 Glean 一起发布。用户可以在
//! `<data_dir>/plugins/<id>/` 下放置同名插件覆盖内置版本（磁盘优先）。
//!
//! 当前内置：
//! - `bilibili`：Bilibili 用户投稿 (Tier 2，wbi 签名)

/// 一个内置插件的静态资源。
pub struct BuiltinPlugin {
    pub id: &'static str,
    pub manifest_toml: &'static str,
    pub adapter_rhai: Option<&'static str>,
}

/// 全部内置插件。
pub fn all() -> &'static [BuiltinPlugin] {
    &[BuiltinPlugin {
        id: "bilibili",
        manifest_toml: include_str!("builtin/bilibili/manifest.toml"),
        adapter_rhai: Some(include_str!("builtin/bilibili/adapter.rhai")),
    }]
}
