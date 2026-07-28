# M0 UI Spike 记录

> 状态：**进行中（有明确 Fail 项，修复中）**  
> 产物：GitHub Actions artifact `glean-spike-windows-x64`  
> 对应方案：[Glean-开发方案.md](./Glean-开发方案.md) §9.0  
> 路径：**A — egui + WebView2**（用户已选定）

## 环境

| 项 | 值 |
|----|-----|
| 测试机 OS | Windows（用户本机） |
| 启动方式 | 用户始终双击 exe；旧构建仍弹黑 CMD（Console 子系统） |
| 构建来源 | Actions artifact |
| 日期 | 2026-07-28 起 |

## 宿主模式

| 模式 | 选用 | 备注 |
|------|------|------|
| H1 Child embed | ☑ | 主路径 |
| H2 Follow overlay | ⚠ | 仍与 H1 同实现 |

## 用户实测（本轮）

| # | 场景 | 结果 | 说明 |
|---|------|------|------|
| 1 | 分隔条 | **Fail→修复中** | 拖左条右条跟着动；拖右条左条不动 |
| 2 | 最大化/最小化/还原 | **Pass** | 用户确认正常 |
| 3 | DPI | ☐ | 未测 |
| 4 | 跨屏 DPI | ☐ | 可 N/A |
| 5 | 中文 IME 候选框 | **Fail** | 无候选框（egui/winit 已知弱项，记入风险） |
| 6 | 列表↔阅读焦点 | ☐ | 未单独测 |
| 7 | 外链 | ☐ | 阅读区空白时无法测 |
| 8 | Stress ×50 内存 | **Pass（观察）** | 未见线性暴涨 |
| — | 点列表后阅读区 | **Fail→修复中** | 空白；疑挂错 HWND（CMD 控制台）或 bounds |
| — | Theme | **Partial→修复中** | 客户区变暗，系统标题栏仍白；WebView HTML 应随主题重载 |

## 根因假设与对策

1. **阅读区空白**：从 CMD 启动时，进程有 Console 窗口；早期 EnumWindows 可能把 WebView 挂到错误父窗。  
   **对策：** 排除 `GetConsoleWindow`；优先标题含 `Glean`/`拾光`；启动时 `FreeConsole`；日志打印 parent title。  
2. **左分隔带动右分隔观感**：左拖改变 nav 后 list 的屏幕起点右移，右分隔“跟着走”。  
   **对策：** 绝对坐标设宽；钳制 `READER_MIN`；左右分隔只改各自宽度。  
3. **标题栏不暗**：DWM `DWMWA_USE_IMMERSIVE_DARK_MODE`。  
4. **IME 无候选框**：记为路径 A 风险；不在本轮强行修完，但门禁表标 Fail。  
5. **libpng iCCP**：依赖/字体或图像元数据警告，**可忽略**，与功能无关。

## 门禁结论

- [ ] **Pass**
- [ ] **Fail → 切 Tauri**（若空白/IME/嵌入在修复后仍不可接受）

当前：**未 Pass**。等下一版 artifact 验证空白与分隔条后再决定。

## 请用新 artifact 再测

1. **尽量资源管理器双击** `glean-spike.exe`（不要只依赖 CMD）  
2. 启动后看是否短暂闪 CMD（FreeConsole 后应脱离）  
3. 点列表 1/2/3，右侧是否出正文  
4. 只拖左分隔：导航变宽，列表起点应右移，但列表**宽度**大致不变  
5. 只拖右分隔：列表变宽，导航宽度不变  
6. Theme：客户区 + 标题栏 + 阅读 HTML 是否都变暗  
7. 搜索框 IME：有无候选框（预期仍可能 Fail）  
