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
        if len(msg) == 0 { msg = str_safe(json_path(j, "body.error.message")); }
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
    let out = s.replace("&", "&amp;");
    let out = out.replace("<", "&lt;");
    let out = out.replace(">", "&gt;");
    let out = out.replace("\"", "&quot;");
    return out.replace("'", "&#39;");
}

fn extract_creator(url) {
    let parts = url.split("/");
    let seen_host = false;
    for p in parts {
        if len(p) == 0 { continue; }
        if p == "fanbox.cc" || p == "www.fanbox.cc" { seen_host = true; continue; }
        if !seen_host { continue; }
        let creator = p;
        if creator.starts_with("@") { creator = creator.substring(1, len(creator)); }
        if len(creator) == 0 { return ""; }
        for c in creator {
            if !((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-' || c == '_') { return ""; }
        }
        return creator;
    }
    return "";
}

fn posts_from_response(json) {
    let posts = json_path(json, "body.items");
    if type_of(posts) != "array" { posts = json_path(json, "body.posts"); }
    if type_of(posts) != "array" { posts = json_path(json, "posts"); }
    if type_of(posts) != "array" { posts = json_path(json, "items"); }
    return posts;
}

fn next_from_response(json) {
    let next = str_safe(json_path(json, "body.nextUrl"));
    if len(next) == 0 { next = str_safe(json_path(json, "body.next_url")); }
    if len(next) == 0 { next = str_safe(json_path(json, "nextUrl")); }
    if len(next) == 0 { next = str_safe(json_path(json, "next_url")); }
    return next;
}

let creator = extract_creator(SOURCE_URL);
if len(creator) == 0 { throw("fanbox: SOURCE_URL 必须是 fanbox.cc/@创作者 或 fanbox.cc/创作者"); }

let hdrs = #{
    "User-Agent": UA,
    "Origin": "https://fanbox.cc",
    "Referer": "https://fanbox.cc/",
};
let limit = 10;
let offset = 0;
let page = 0;
let max_pages = 20;
let url = "https://api.fanbox.cc/post.listCreator?creatorId=" + creator + "&limit=" + limit;
let count = 0;
let author = "";

while page < max_pages {
    page += 1;
    let resp = http_get(url, hdrs, "fanbox_session");
    let status = resp["status"];
    if status != 200 {
        let detail = extract_api_error(resp);
        if status == 401 || status == 403 { throw("fanbox posts HTTP " + status + "：fanbox_session 无效、未设置或没有访问权限"); }
        if status == 429 { throw("fanbox posts HTTP 429：请求过于频繁，请稍后刷新"); }
        if status == 0 { throw("fanbox posts 请求失败：请先配置 fanbox_session，并检查代理或网络；" + str_safe(resp["error"])); }
        throw("fanbox posts HTTP " + status + " " + detail);
    }
    let json = parse_json(resp["body"]);
    let posts = posts_from_response(json);
    if type_of(posts) != "array" { throw("fanbox posts 响应结构异常：未找到 posts/items 数组"); }
    if len(posts) == 0 { break; }
    let page_count = 0;
    for item in posts {
        let id = str_safe(json_path(item, "id"));
        if len(id) == 0 { continue; }
        let title = str_safe(json_path(item, "title"));
        if len(title) == 0 { title = "Fanbox 投稿 " + id; }
        let text = str_safe(json_path(item, "body.text"));
        if len(text) == 0 { text = str_safe(json_path(item, "excerpt")); }
        let item_author = str_safe(json_path(item, "creator.name"));
        if len(item_author) == 0 { item_author = str_safe(json_path(item, "user.name")); }
        if len(author) == 0 && len(item_author) > 0 { author = item_author; }
        let published = iso_to_unix(str_safe(json_path(item, "publishedDatetime")));
        if published == 0 { published = iso_to_unix(str_safe(json_path(item, "published_at"))); }
        let post_url = "https://fanbox.cc/@" + creator + "/posts/" + id;
        let image = str_safe(json_path(item, "coverImageUrl"));
        if len(image) == 0 { image = str_safe(json_path(item, "cover.imageUrl")); }
        let html = "";
        if len(text) > 0 { html = "<p>" + html_escape(text).replace("\n", "</p><p>") + "</p>"; }
        if len(image) > 0 { html += "<p><img src=\"" + html_escape(image) + "\"></p>"; }
        set_field("guid", "fanbox-" + id);
        set_field("title", title);
        set_field("url", post_url);
        set_field("author", item_author);
        if published > 0 { set_field("published_at", published); }
        if len(text) > 0 { set_field("summary", text); }
        set_field("content_html", html);
        if len(image) > 0 { set_field("thumbnail", image); }
        add_entry();
        count += 1;
        page_count += 1;
    }
    log("info", "fanbox creator=" + creator + " 第 " + page + " 页 +" + page_count + " 条，累计 " + count);
    let next = next_from_response(json);
    if len(next) > 0 {
        if !next.starts_with("https://api.fanbox.cc/") { break; }
        url = next;
    } else {
        if page_count < limit { break; }
        offset += page_count;
        url = "https://api.fanbox.cc/post.listCreator?creatorId=" + creator + "&limit=" + limit + "&offset=" + offset;
    }
    sleep_ms(300);
}

if page >= max_pages { log("warn", "fanbox 已达分页上限 " + max_pages + " 页"); }
if len(author) > 0 { set_feed_title("Fanbox " + author); }
log("info", "fanbox creator=" + creator + " 收集 " + count + " 条");
