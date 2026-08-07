// Fantia 创作者投稿适配器（Tier 2 脚本）。
//
// 数据获取方式：
//   1. 帖子列表（HTML 页面）：
//      GET https://fantia.jp/fanclubs/{id}/posts?page={N}
//      → HTML，通过正则提取 /posts/{id} 链接获取帖子 ID 列表
//
//   2. 帖子详情（JSON API）：
//      GET https://fantia.jp/api/v1/posts/{post_id}
//      → { post: { title, comment, posted_at, fanclub: { creator_name },
//                  thumb: { main, thumb }, post_contents: [...] } }
//
//   3. 创作者信息（JSON API）：
//      GET https://fantia.jp/api/v1/fanclubs/{id}
//      → { fanclub: { creator_name, name, user: { name } } }
//
// 限流策略：
//   Fantia 的 429 是按 IP 封锁，冷却期极长（半小时以上），继续请求只会
//   延长封锁。因此：
//   - 宿主层已禁用 fantia.jp 的 429 自动重试（runtime.rs）
//   - 适配器层：429 立即 throw 终止，不继续后续页面或投稿
//   - 请求间加入延迟（1-2 秒），降低触发封锁的概率
//   - 单次刷新限制最大分页数（5 页）

const UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

fn str_safe(v) {
    let t = type_of(v);
    if t == "string" { return v; }
    if t == "i64" || t == "i32" || t == "int" || t == "f64" { return "" + v; }
    return "";
}

fn parse_int_safe(v) {
    if type_of(v) == "i64" || type_of(v) == "i32" || type_of(v) == "int" { return v; }
    let s = str_safe(v);
    if len(s) == 0 { return 0; }
    try { return s.parse_int(); } catch { return 0; }
}

fn iso_to_unix(s) {
    if type_of(s) != "string" || len(s) < 19 { return 0; }
    let ymd = s.substring(0, 10).split("-");
    let hms = s.substring(11, 19).split(":");
    if len(ymd) != 3 || len(hms) != 3 { return 0; }
    let y = parse_int_safe(ymd[0]);
    let mo = parse_int_safe(ymd[1]);
    let d = parse_int_safe(ymd[2]);
    let h = parse_int_safe(hms[0]);
    let mi = parse_int_safe(hms[1]);
    let se = parse_int_safe(hms[2]);
    if y < 2000 || y > 2100 || mo < 1 || mo > 12 || d < 1 || d > 31 { return 0; }
    let y2 = y - (if mo <= 2 { 1 } else { 0 });
    let era = (if y2 >= 0 { y2 } else { y2 - 399 }) / 400;
    let yoe = y2 - era * 400;
    let mp = mo + (if mo > 2 { -3 } else { 9 });
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let ts = days * 86400 + h * 3600 + mi * 60 + se;
    let tz = s.substring(19, len(s));
    if tz.starts_with("+") || tz.starts_with("-") {
        let sign = if tz.starts_with("+") { 1 } else { -1 };
        let parts = tz.substring(1, len(tz)).split(":");
        if len(parts) >= 2 { return ts - sign * (parse_int_safe(parts[0]) * 3600 + parse_int_safe(parts[1]) * 60); }
    }
    return ts;
}

fn html_escape(s) {
    if type_of(s) != "string" { return ""; }
    let out = s;
    out.replace("&", "&amp;");
    out.replace("<", "&lt;");
    out.replace(">", "&gt;");
    out.replace("\"", "&quot;");
    out.replace("'", "&#39;");
    return out;
}

/// 从 SOURCE_URL 提取 fanclub ID。
/// 支持 https://fantia.jp/fanclubs/{id} 和 https://fantia.jp/@{name} 两种格式。
fn extract_fanclub_id(url) {
    if url.contains("/fanclubs/") {
        let parts = url.split("/fanclubs/");
        if len(parts) >= 2 {
            let id = "";
            for ch in parts[1] {
                if ch >= '0' && ch <= '9' { id += ch; } else { break; }
            }
            if len(id) > 0 { return id; }
        }
    }
    if url.contains("/@") {
        let parts = url.split("/@");
        if len(parts) >= 2 {
            let clean = "";
            for ch in parts[1] {
                if ch == '?' || ch == '#' || ch == '/' { break; }
                clean += ch;
            }
            if len(clean) > 0 { return clean; }
        }
    }
    return "";
}

/// 从 HTML 页面中提取帖子 ID 列表。
/// Fantia 的帖子列表页是 HTML，帖子链接格式为 /posts/{数字ID}。
/// 列表页 HTML 很大，逐字符扫描会触发 Rhai 操作数上限，因此由宿主
/// `extract_fantia_post_ids` 在 Rust 侧高效解析并返回去重后的 ID 数组。
fn extract_post_ids_from_html(html) {
    return extract_fantia_post_ids(html);
}

/// 检查 HTTP 响应状态。429 时立即终止（Fantia 的 IP 封锁不可重试）。
/// 返回 true = 正常继续，false = 应终止（已 throw）。
fn check_status(resp, context) {
    let status = resp["status"];
    if status == 200 { return true; }
    if status == 429 {
        throw("fantia " + context + " HTTP 429：IP 被临时封锁，请等待较长时间（30分钟以上）后再试。"
            + " Fantia 的限流按 IP 计算，继续请求只会延长封锁时间。");
    }
    if status == 401 || status == 403 {
        throw("fantia " + context + " HTTP " + status + "：fantia_session 无效、未设置或没有访问权限");
    }
    if status == 404 {
        throw("fantia " + context + " HTTP 404：fanclub 不存在或 ID 错误");
    }
    if status == 0 {
        throw("fantia " + context + " 请求失败：请先配置 fantia_session，并检查代理或网络；" + str_safe(resp["error"]));
    }
    let body = str_safe(resp["body"]);
    let clip = if len(body) > 180 { body.substring(0, 180) } else { body };
    throw("fantia " + context + " HTTP " + status + " " + clip);
}

let fanclub_id = extract_fanclub_id(SOURCE_URL);
if len(fanclub_id) == 0 {
    throw("fantia: SOURCE_URL 必须是 fantia.jp/fanclubs/{id} 或 fantia.jp/@{name}");
}

let hdrs = #{
    "User-Agent": UA,
    "Origin": "https://fantia.jp",
    "Referer": "https://fantia.jp/",
    "Accept": "text/html,application/xhtml+xml,application/json",
};

// Fantia 限流比 pixiv/fanbox 严格得多：连续 4-5 个用户即触发 IP 封锁。
let max_pages = 5;
let max_posts = 20;
let count = 0;
let author = "";
let shallow = SHALLOW_MODE == "true";
let existing = EXISTING_GUIDS;

// 获取创作者名称（1 次 API 请求）。
let fc_resp = http_get("https://fantia.jp/api/v1/fanclubs/" + fanclub_id, hdrs, "fantia_session");
if fc_resp["status"] == 200 {
    try {
        let fc_json = parse_json(fc_resp["body"]);
        let fc = json_path(fc_json, "fanclub");
        if type_of(fc) == "map" {
            author = str_safe(json_path(fc, "creator_name"));
            if len(author) == 0 { author = str_safe(json_path(fc, "name")); }
            if len(author) == 0 { author = str_safe(json_path(fc, "user.name")); }
        }
    } catch {}
}

let page = 0;
let has_more = true;

while has_more && page < max_pages && count < max_posts {
    page += 1;
    // 1) 拉取帖子列表页（HTML）。
    let list_url = "https://fantia.jp/fanclubs/" + fanclub_id + "/posts?page=" + page;
    let resp = http_get(list_url, hdrs, "fantia_session");
    if !check_status(resp, "posts page=" + page) { break; }

    let html = str_safe(resp["body"]);
    let post_ids = extract_post_ids_from_html(html);
    if len(post_ids) == 0 { break; }

    let page_count = 0;
    let page_has_overlap = false;
    for id in post_ids {
        if count >= max_posts { break; }
        let guid = "fantia-" + id;
        // 浅检模式：已有条目 → 跳过详情请求（省 HTTP），标记重叠。
        if len(existing) > 0 && existing.contains("," + guid + ",") {
            page_has_overlap = true;
            continue;
        }

        // 2) 拉取帖子详情（JSON API）。
        let post_url = "https://fantia.jp/posts/" + id;
        let title = "";
        let text = "";
        let thumb = "";
        let published = 0;
        let images = [];
        let content_html = "";

        // 请求间延迟：降低触发 IP 封锁的概率。
        sleep_ms(1000 + (parse_int_safe(id) % 1000));
        let info_resp = http_get("https://fantia.jp/api/v1/posts/" + id, hdrs, "fantia_session");
        // 429 → 立即终止。
        if info_resp["status"] == 429 {
            throw("fantia post.detail HTTP 429：IP 被临时封锁，请等待较长时间（30分钟以上）后再试。");
        }
        if info_resp["status"] == 200 {
            try {
                let info_json = parse_json(info_resp["body"]);
                let post = json_path(info_json, "post");
                if type_of(post) != "map" { post = info_json; }
                title = str_safe(json_path(post, "title"));
                text = str_safe(json_path(post, "comment"));
                published = iso_to_unix(str_safe(json_path(post, "posted_at")));

                let p_author = str_safe(json_path(post, "fanclub.creator_name"));
                if len(p_author) == 0 { p_author = str_safe(json_path(post, "fanclub.user.name")); }
                if len(author) == 0 && len(p_author) > 0 { author = p_author; }

                thumb = str_safe(json_path(post, "thumb.main"));
                if len(thumb) == 0 { thumb = str_safe(json_path(post, "thumb.thumb")); }

                // 提取图片：post_contents 数组中的 photo_gallery 类型。
                let contents = json_path(post, "post_contents");
                if type_of(contents) == "array" {
                    for content in contents {
                        let cat = str_safe(json_path(content, "category"));
                        if cat == "photo_gallery" {
                            let photos = json_path(content, "post_content_photos");
                            if type_of(photos) == "array" {
                                for photo in photos {
                                    let url = str_safe(json_path(photo, "url.original"));
                                    if len(url) == 0 { url = str_safe(json_path(photo, "url.main")); }
                                    if len(url) > 0 { images.push(url); }
                                }
                            }
                        }
                    }
                }
            } catch {}
        }

        sleep_ms(1000 + (parse_int_safe(id) % 1000));
        let page_resp = http_get(post_url, hdrs, "fantia_session");
        if page_resp["status"] == 429 {
            throw("fantia post.page HTTP 429：IP 被临时封锁，请等待较长时间（30分钟以上）后再试。");
        }
        if page_resp["status"] == 200 {
            content_html = extract_fantia_post_contents(str_safe(page_resp["body"]));
        }

        if len(title) == 0 { title = "Fantia 投稿 " + id; }

        // 3) 页面主体提取失败时，使用 API 字段组装最小正文。
        if len(content_html) == 0 && len(text) > 0 {
            let escaped_text = html_escape(text);
            escaped_text.replace("\n", "</p><p>");
            content_html = "<p>" + escaped_text + "</p>";
        }
        if len(content_html) == 0 {
            for img in images {
                content_html += "<p><img src=\"" + html_escape(img) + "\"></p>";
            }
            if len(content_html) == 0 && len(thumb) > 0 {
                content_html = "<p><img src=\"" + html_escape(thumb) + "\"></p>";
            }
        }

        set_field("guid", guid);
        set_field("title", title);
        set_field("url", post_url);
        set_field("author", author);
        if published > 0 { set_field("published_at", published); }
        if len(text) > 0 { set_field("summary", text); }
        set_field("content_html", content_html);
        if len(thumb) > 0 { set_field("thumbnail", thumb); }
        add_entry();
        count += 1;
        page_count += 1;
    }

    if shallow && page_has_overlap {
        log("info", "fantia 第 " + page + " 页命中已有内容，浅检停止分页");
        break;
    }

    log("info", "fantia fanclub=" + fanclub_id + " 第 " + page + " 页 +" + page_count + " 条，累计 " + count);

    // Fantia 每页约 20-30 条帖子，如果本页有帖子则尝试下一页。
    has_more = len(post_ids) >= 20;

    if has_more && page < max_pages && count < max_posts {
        sleep_ms(2000);
    }
}

if page >= max_pages { log("warn", "fantia 已达分页上限 " + max_pages + " 页（防止 IP 封锁）"); }
if count >= max_posts { log("warn", "fantia 已达条目上限 " + max_posts + " 条"); }
if len(author) > 0 { set_feed_title("Fantia " + author); }
log("info", "fantia fanclub=" + fanclub_id + " 收集 " + count + " 条");
