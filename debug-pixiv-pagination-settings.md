# Pixiv 分页与设置持久化调试

状态：[OPEN]

## 现象

- Pixiv 列表只显示第一页约 30 条，没有后续分页。
- 主题、弹窗大小和位置等状态重启后恢复默认。

## 假设

1. 运行中的 Pixiv 插件不是仓库中的最新版本。
2. Pixiv API 的 `next_url` 为空或被域名校验拒绝。
3. 多页结果在写入数据库或刷新列表时被截断。
4. 配置保存路径与启动读取路径不一致，或配置解析失败后回退默认值。
5. 弹窗状态没有进入持久化配置，而只保存在 egui 临时状态中。

## 证据

| 假设 | 运行时证据 | 结论 |
| --- | --- | --- |
| 1 | 待采集 | 待定 |
| 2 | 待采集 | 待定 |
| 3 | 待采集 | 待定 |
| 4 | 待采集 | 待定 |
| 5 | 待采集 | 待定 |

## 复现步骤

1. 启动调试构建。
2. 刷新 Pixiv 订阅。
3. 关闭并重新启动程序。
4. 检查 Pixiv 条目数、主题、弹窗尺寸和位置。

## 首份证据（用户日志）

关键结论：

- `[config-load]` / `[config-save]` 始终使用同一路径
  `C:\Users\tomcb\AppData\Roaming\Glean\config.json`，且 `dark`、窗口尺寸位置与
  `maximized` 均成功写入并读回。
- Pixiv 分页正常：第 1~20 页累计 `587` 条（`pixiv 收集 587 条`）。
- **界面仍只显示 30 条、主题/窗体状态不恢复**。

因此已排除“插件仍为旧版只拉首屏”的假设：Pixiv 适配器确实返回 587 条。
真正问题位于「写入数据库 → 重新查询列表 → UI 显示」链路，需用新增日志定位：

- `[refresh-updated] feed_id=… parsed=… new_items=…`
- `[entries-updated] filter=… search=… count=…`
- `[ui-entries-updated] count=…`

另已修正：启动时改为调用真正执行 DWM 标题栏主题的
`set_titlebar_dark(s.dark)`（此前只用 `set_dark_title` 设置内部标志）。

