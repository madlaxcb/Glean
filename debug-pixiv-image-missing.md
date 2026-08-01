# Debug Session: pixiv-image-missing
- **Status**: [CLOSED] 2026-08-01
- **Issue**: Pixiv 条目可以获取，但阅读器仍不显示图片
- **Debug Server**: http://127.0.0.1:7777/event
- **Log File**: .dbg/trae-debug-log-pixiv-image-missing.ndjson

## Reproduction Steps
1. 安装最新 GitHub Actions 构建产物。
2. 打开 Pixiv 用户订阅中的作品。
3. 若图片策略为按需加载，点击「显示图片」。
4. 观察图片仍为空白。

## Hypotheses & Verification
| ID | Hypothesis | Likelihood | Effort | Evidence |
|----|------------|------------|--------|----------|
| H1 | Pixiv CDN 下载返回 403、网络错误或代理连接失败 | High | Low | Pending |
| H2 | 下载成功，但缓存写入或 HTML 重写失败 | High | Low | Pending |
| H3 | glean-img 自定义协议 URL 的 authority/path 解析不稳定 | High | Low | Confirmed by broken placeholder and copied glean-img URL |
| H4 | WebView2 CSP 仍拦截 glean-img 请求 | Medium | Medium | Pending |
| H5 | Ammonia 二次清洗移除 glean-img URL | High | Low | Confirmed |
| H6 | 当前配置为 Block，主动移除所有图片 | High | Low | Screenshot strongly indicates; user must verify setting |
| H7 | 已有同 GUID 条目刷新时不更新无图正文 | High | Low | Confirmed |
| H8 | 残留全文抽取正文遮盖插件正文 | Low | Low | Rejected: screenshot shows extraction failed; failure does not write extracted_html |

## Log Evidence
- pre-fix: input=`<img src="glean-img://abc123.jpg">`, output=`<img>`, scheme_preserved=false。
- 结论：最终 reader 渲染的二次清洗删除了自定义协议 src，WebView 无法请求缓存图片。
- H7 pre-fix: second_inserted=false, stored_content=`<p>caption</p>`, new_image_preserved=false。
- H7 post-fix: second_inserted=false, stored_content 包含 `i.pximg.net`, new_image_preserved=true。
- 用户验证：正文出现图片占位符，复制地址为 `glean-img://50ef2c4a4a047187.jpg`；图片标签与缓存重写已生效，失败点进入自定义协议读取。
- H3 修复：新 URL 使用 `glean-img:///filename`，handler 兼容 authority/path 及 WebView2 localhost 形式。
- 用户在 `0d59e07` 确认图片仍为破图，复制地址为 `glean-img:///d5da7054b01b94c9.jpg`。
- H3 根因更新：Windows WebView2 的 Wry 协议拦截需要非空 authority；空 authority 的三斜杠 URL 无法稳定命中 `http://glean-img.*` 过滤器。
- H3 修复：新 URL 改为 `glean-img://localhost/filename`，与 Wry 的 `http://glean-img.localhost/filename` 映射一致。
- 用户在 `cbf5dd4` 确认 `glean-img://localhost/...` 仍为破图；自定义协议桥接判定为不可靠。
- 新渲染路径：图片下载并落盘缓存后直接改写为 `data:image/...;base64,...`，绕过 Windows WebView2/Wry 的自定义协议资源分发。
- 用户在 `310f4b9` 确认连破图元素也不存在；H5 的同类问题再次命中：Ammonia 默认不允许 `data:`，最终清洗删除了内联图片 src。
- 修复：仅在 `ImagePolicy::Allow` 时允许 `data:` URL scheme。

## Verification Conclusion
- pre-fix：`glean-img://` 被清洗为无 src 的 `<img>`。
- post-fix：`glean-img://` 完整保留，回归测试通过。
- H7 修复后：已有条目刷新会更新正文，但保留已读、星标及全文抽取状态。
- 完整核心测试：127 passed，3 ignored；`cargo check` 和 rustfmt 通过。
- H3 回归测试覆盖 4 种 WebView URL 形式；glean-app 5 tests、glean-core 127 tests 通过。
- Pixiv artwork URL 不再触发无意义的全文抽取。
- `custom_scheme_uses_localhost_authority` 与 reader URI 解析回归测试通过。
- `data_url_encodes_cached_image` 与全部 image_cache 测试通过。
- `allow_policy_keeps_inlined_image_data` 回归测试通过；Block 和按需策略仍会移除图片标签。
- 用户在 `c855780` 确认图片可显示。
- 后续修复（原图内联过大导致空白、改本机 HTTP 服务提供原图 `30aa395`）均确认生效。
- 分页拉取（`61dddd2`）与 Rhai 操作数上限（`1954c67`）修复完成。
- **关闭原因：图片显示问题已解决，调试插桩已清理。**
