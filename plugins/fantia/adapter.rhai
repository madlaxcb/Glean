// Fantia 创作者投稿适配器（Tier 2 脚本）。
//
// API：
//   GET https://fantia.jp/api/v1/fanclubs/{id}/posts?page={N}
//     → { posts: [{ id, title, published_at, fanclub: { ... } }] }
//   GET https://fantia.jp/api/v1/posts/{post_id}
//     → { post: { title, comment, body: { sections: [...] }, thumb: { ... } } }
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

fn extract_api_error(resp) {
    let body = str_safe(resp["body"]);
    if len(body) == 0 { return ""; }
    let clip = if len(body) > 180 { body.substring(0, 180) } else { body };
    try {
        let j = parse_json(body);
        let msg = str_safe(json_path(j, "error.message"));
        if len(msg) == 0 { msg = str_safe(json_path(j, "message")); }
        if len(msg) == 0 { msg = clip; }
        return msg;
    } catch { return clip; }
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
/// 对于 @name 格式，name 本身可能就是 fanclub 的 slug，直接用于 API。
fn extract_fanclub_id(url) {
    // 匹配 fantia.jp/fanclubs/{id}
    if url.contains("/fanclubs/") {
        let parts = url.split("/fanclubs/");
        if len(parts) >= 2 {
            let id_part = parts[1];
            // 取纯数字部分（去掉 query string 等）
            let id = "";
            for ch in id_part {
                if ch >= '0' && ch <= '9' { id += ch; } else { break; }
            }
            if len(id) > 0 { return id; }
        }
    }
    // 匹配 fantia.jp/@{name}
    if url.contains("/@") {
        let parts = url.split("/@");
        if len(parts) >= 2 {
            let name = parts[1];
            // 去掉 query string
            let clean = "";
            for ch in name {
                if ch == '?' || ch == '#' { break; }
                clean += ch;
            }
            if len(clean) > 0 { return clean; }
        }
    }
    return "";
}

/// 检查 HTTP 响应状态。429 时立即终止（Fantia 的 IP 封锁不可重试）。
/// 返回 true = 正常继续，false = 应终止（已 throw）。
fn check_status(resp, context) {
    let status = resp["status"];
    if status == 200 { return true; }
    let detail = extract_api_error(resp);
    if status == 429 {
        // Fantia 的 429 是 IP 级封锁，冷却期极长（半小时以上）。
        // 不重试、不继续，直接终止并告知用户。
        throw("fantia " + context + " HTTP 429：IP 被临时封锁，请等待较长时间（30分钟以上）后再试。"
            + " Fantia 的限流按 IP 计算，继续请求只会延长封锁时间。");
    }
    if status == 401 || status == 403 {
        throw("fantia " + context + " HTTP " + status + "：fantia_session 无效、未设置或没有访问权限");
    }
    if status == 0 {
        throw("fantia " + context + " 请求失败：请先配置 fantia_session，并检查代理或网络；" + str_safe(resp["error"]));
    }
    throw("fantia " + context + " HTTP " + status + " " + detail);
}

let fanclub_id = extract_fanclub_id(SOURCE_URL);
if len(fanclub_id) == 0 {
    throw("fantia: SOURCE_URL 必须是 fantia.jp/fanclubs/{id} 或 fantia.jp/@{name}");
}

let hdrs = #{
    "User-Agent": UA,
    "Origin": "https://fantia.jp",
    "Referer": "https://fantia.jp/",
    "Accept": "application/json",
};

// Fantia 限流比 pixiv/fanbox 严格得多：连续 4-5 个用户即触发 IP 封锁。
// 限制单次刷新的最大分页数和条目数。
let max_pages = 5;
let max_posts = 20;
let count = 0;
let author = "";
let shallow = SHALLOW_MODE == "true";
let existing = EXISTING_GUIDS;

let page = 0;
let has_more = true;

while has_more && page < max_pages && count < max_posts {
    page += 1;
    // 1) 拉取投稿列表页。
    let list_url = "https://fantia.jp/api/v1/fanclubs/" + fanclub_id + "/posts?page=" + page;
    let resp = http_get(list_url, hdrs, "fantia_session");
    // 429 → check_status 内部直接 throw 终止（不继续后续页面）。
    if !check_status(resp, "posts page=" + page) { break; }

    let json = parse_json(resp["body"]);
    let posts = json_path(json, "posts");
    if type_of(posts) != "array" { posts = json_path(json, "data.posts"); }
    if type_of(posts) != "array" { throw("fantia posts 响应结构异常：未找到 posts 数组"); }
    if len(posts) == 0 { break; }

    let page_count = 0;
    let page_has_overlap = false;
    for item in posts {
        if count >= max_posts { break; }
        let id = str_safe(json_path(item, "id"));
        if len(id) == 0 { continue; }

        let guid = "fantia-" + id;
        // 浅检模式：已有条目 → 跳过 post 详情请求（省 HTTP），标记重叠。
        if len(existing) > 0 && existing.contains("," + guid + ",") {
            page_has_overlap = true;
            continue;
        }

        let title = str_safe(json_path(item, "title"));
        if len(title) == 0 { title = "Fantia 投稿 " + id; }

        let item_author = str_safe(json_path(item, "fanclub.creator_name"));
        if len(item_author) == 0 { item_author = str_safe(json_path(item, "fanclub.user.name")); }
        if len(author) == 0 && len(item_author) > 0 { author = item_author; }

        let published = iso_to_unix(str_safe(json_path(item, "published_at")));
        if published == 0 { published = iso_to_unix(str_safe(json_path(item, "posted_at"))); }

        let post_url = "https://fantia.jp/posts/" + id;
        let thumb = str_safe(json_path(item, "thumb.main"));
        if len(thumb) == 0 { thumb = str_safe(json_path(item, "thumb.thumb")); }

        // 2) 拉取完整正文（post detail）。列表接口不含正文。
        let text = "";
        let images = [];
        // 请求间延迟：降低触发 IP 封锁的概率。
        sleep_ms(1000 + (id.len() % 1000));
        let info_resp = http_get("https://fantia.jp/api/v1/posts/" + id, hdrs, "fantia_session");
        // 429 → 立即终止（不继续后续投稿）。
        if info_resp["status"] == 429 {
            throw("fantia post.info HTTP 429：IP 被临时封锁，请等待较长时间（30分钟以上）后再试。");
        }
        if info_resp["status"] == 200 {
            let info_json = parse_json(info_resp["body"]);
            let post = json_path(info_json, "post");
            if type_of(post) != "map" { post = info_json; }
            text = str_safe(json_path(post, "comment"));
            if len(text) == 0 { text = str_safe(json_path(post, "body.text")); }

            // 提取图片：body.sections 中的 photo 类型。
            let sections = json_path(post, "body.sections");
            if type_of(sections) == "array" {
                for sec in sections {
                    let sec_type = str_safe(json_path(sec, "post_content_type"));
                    if sec_type == "photo" || sec_type == "image" {
                        let img_url = str_safe(json_path(sec, "post_content_photos.0.url"));
                        if len(img_url) == 0 { img_url = str_safe(json_path(sec, "url")); }
                        if len(img_url) > 0 { images.push(img_url); }
                    }
                }
            }
        }

        if len(text) == 0 { text = str_safe(json_path(item, "excerpt")); }
        if len(text) == 0 { text = str_safe(json_path(item, "comment")); }

        // 3) 组装 HTML。
        let html = "";
        if len(text) > 0 {
            let escaped_text = html_escape(text);
            escaped_text.replace("\n", "</p><p>");
            html = "<p>" + escaped_text + "</p>";
        }
        for img in images {
            html += "<p><img src=\"" + html_escape(img) + "\"></p>";
        }
        if len(html) == 0 && len(thumb) > 0 { html = "<p><img src=\"" + html_escape(thumb) + "\"></p>"; }

        set_field("guid", guid);
        set_field("title", title);
        set_field("url", post_url);
        set_field("author", item_author);
        if published > 0 { set_field("published_at", published); }
        if len(text) > 0 { set_field("summary", text); }
        set_field("content_html", html);
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

    // 判断是否还有更多页：Fantia 的分页通过 has_more 或 posts 数组长度判断。
    let more = json_path(json, "has_more");
    if type_of(more) == "bool" {
        has_more = more;
    } else {
        // 如果本页满了（通常 30 条/页），可能还有下一页。
        has_more = len(posts) >= 30;
    }

    // 分页间延迟：降低触发 IP 封锁的概率。
    if has_more && page < max_pages && count < max_posts {
        sleep_ms(2000);
    }
}

if page >= max_pages { log("warn", "fantia 已达分页上限 " + max_pages + " 页（防止 IP 封锁）"); }
if count >= max_posts { log("warn", "fantia 已达条目上限 " + max_posts + " 条"); }
if len(author) > 0 { set_feed_title("Fantia " + author); }
log("info", "fantia fanclub=" + fanclub_id + " 收集 " + count + " 条");
