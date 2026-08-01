// Pixiv 用户投稿 Tier 2 适配器。
//
// 流程：
//   1. refresh_token → access_token（POST oauth.secure.pixiv.net/auth/token）。
//      请求体中的 {{pixiv_refresh_token}} 由 Host 在发送前替换为凭证值，
//      脚本永远拿不到明文（§11.5.9）。
//   2. access_token → app-api.pixiv.net/v1/user/illusts 拉用户作品列表。
//
// SOURCE_URL 由 Host 通过 Rhai 全局常量注入（见 runtime.rs::run_script），
// 形如 "https://www.pixiv.net/users/2948577"。
//
// 凭证：插件管理 → pixiv → 凭证槽 pixiv_refresh_token（Pixiv OAuth Refresh Token）。
// 未设置或失效时会 throw，刷新失败原因会显示在 UI。

const UA = "PixivAndroidApp/5.0.234 (Android 11; Pixel 5)";

fn parse_int_safe(s) {
    if type_of(s) != "string" || len(s) == 0 { return 0; }
    try {
        return s.parse_int();
    } catch {
        return 0;
    }
}

/// 安全转字符串：string 原样，i64 转数字串，其余返回 ""。
fn str_safe(v) {
    let t = type_of(v);
    if t == "string" { return v; }
    if t == "i64" || t == "i32" || t == "int" { return "" + v; }
    return "";
}

/// 从 HTTP 非 200 的响应中提取 Pixiv 返回的错误详情（body JSON）。
/// Pixiv 常见格式：errors.system.message / error.message / error。
fn extract_api_error(resp) {
    let body = str_safe(resp["body"]);
    if len(body) == 0 { return ""; }
    let clip = if len(body) > 160 { body.substring(0, 160) } else { body };
    try {
        let j = parse_json(body);
        if type_of(j) != "map" { return clip; }
        let msg = str_safe(json_path(j, "errors.system.message"));
        if len(msg) == 0 { msg = str_safe(json_path(j, "error.message")); }
        if len(msg) == 0 { msg = str_safe(json_path(j, "error")); }
        if len(msg) == 0 { msg = clip; }
        return msg;
    } catch {
        return clip;
    }
}

/// ISO8601 → unix 秒（忽略时区偏移，取本地字段）。失败返回 0。
fn iso_to_unix(s) {
    if type_of(s) != "string" || len(s) < 19 { return 0; }
    let ymd = s.substring(0, 10).split("-");
    if len(ymd) != 3 { return 0; }
    let hms = s.substring(11, 19).split(":");
    if len(hms) != 3 { return 0; }
    let y = parse_int_safe(ymd[0]);
    let mo = parse_int_safe(ymd[1]);
    let d = parse_int_safe(ymd[2]);
    let h = parse_int_safe(hms[0]);
    let mi = parse_int_safe(hms[1]);
    let se = parse_int_safe(hms[2]);
    if y < 2000 || y > 2100 || mo < 1 || mo > 12 { return 0; }
    // days_from_civil（Howard Hinnant）转 unix 秒
    let y2 = y - (if mo <= 2 { 1 } else { 0 });
    let era = (if y2 >= 0 { y2 } else { y2 - 399 }) / 400;
    let yoe = y2 - era * 400;
    let mp = mo + (if mo > 2 { -3 } else { 9 });
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    return days * 86400 + h * 3600 + mi * 60 + se;
}

fn extract_user_id(url) {
    let parts = url.split("/");
    for p in parts {
        if len(p) == 0 { continue; }
        let all_digit = true;
        for c in p {
            if c < '0' || c > '9' { all_digit = false; break; }
        }
        if all_digit { return p; }
    }
    return "";
}

/// refresh_token → access_token。连接失败（status=0）自动重试 3 次。
/// 注意：Rhai 函数内不能引用顶层 const（变量作用域限制），常量需内联。
fn get_access_token() {
    let hdrs = #{
        "User-Agent": "PixivAndroidApp/5.0.234 (Android 11; Pixel 5)",
        "Content-Type": "application/x-www-form-urlencoded; charset=UTF-8",
        "App-OS": "android",
        "App-OS-Version": "11",
        "App-Version": "5.0.234",
    };
    let body = "client_id=MOBrBDS8blbauoSck0ZfDbtuzpyT"
        + "&client_secret=lsACyCD94FhDUtGTXi3QzcFE2uU1hqtDaKeqrdwj"
        + "&grant_type=refresh_token"
        + "&get_secure_url=1"
        + "&include_policy=true"
        + "&refresh_token={{pixiv_refresh_token}}";
    let tries = 3;
    let attempt = 0;
    while attempt < tries {
        let resp = http_post(
            "https://oauth.secure.pixiv.net/auth/token",
            body,
            hdrs,
            "pixiv_refresh_token"
        );
        if resp["status"] != 200 {
            if resp["status"] == 0 && attempt < tries - 1 {
                log("warn", "pixiv oauth/token 连接失败，第 " + attempt + " 次重试");
                sleep_ms(1000);
                attempt += 1;
                continue;
            }
            if resp["status"] == 0 {
                throw("pixiv oauth/token 连接失败 error=" + resp["error"]
                    + "（请检查 1) 代理服务是否正在运行 2) 设置页代理地址是否正确 3) 插件已勾选「使用代理」）");
            }
            // 非 200：读取 Pixiv 返回的具体错误（400 多为 Refresh Token 无效）。
            let detail = extract_api_error(resp);
            throw("pixiv oauth/token HTTP " + resp["status"] + " " + detail
                + "（400/401 多为 Refresh Token 无效或已过期，请重新获取后更新凭证）");
        }
        let json = parse_json(resp["body"]);
        if type_of(json) != "map" {
            throw("pixiv oauth/token 响应解析失败");
        }
        let token = json_path(json, "response.access_token");
        if type_of(token) != "string" || len(token) == 0 {
            throw("pixiv oauth/token 未返回 access_token（请检查凭证槽 pixiv_refresh_token 是否有效）");
        }
        return token;
    }
    throw("pixiv oauth/token 连接多次失败");
}

let mid = extract_user_id(SOURCE_URL);
if len(mid) == 0 {
    throw("pixiv: 无法从 SOURCE_URL 提取 user_id: " + SOURCE_URL);
}
log("info", "pixiv user_id=" + mid);

let token = get_access_token();
let hdrs = #{
    "User-Agent": UA,
    "Authorization": "Bearer " + token,
};
let url = "https://app-api.pixiv.net/v1/user/illusts?user_id=" + mid + "&type=illust";
let resp = http_get(url, hdrs, "");
if resp["status"] != 200 {
    let hint = "";
    if resp["status"] == 0 {
        hint = "（连接失败：请检查代理服务是否运行、设置页代理地址是否正确）";
    }
    throw("pixiv user/illusts HTTP " + resp["status"] + " error=" + resp["error"] + hint);
}
let json = parse_json(resp["body"]);
if type_of(json) != "map" {
    throw("pixiv user/illusts 响应解析失败");
}
// app-api.pixiv.net/v1/user/illusts 返回 {"illusts":[...],"next_url":"..."}，
// 顶层无 response 包层（response 包层是 oauth/token 接口的格式）。
let illusts = json_path(json, "illusts");
if type_of(illusts) != "array" {
    // 兜底：兼容万一返回 response.illusts 结构
    let nested = json_path(json, "response.illusts");
    if type_of(nested) == "array" {
        illusts = nested;
    } else {
        let clip = str_safe(resp["body"]);
        if len(clip) > 200 { clip = clip.substring(0, 200); }
        throw("pixiv user/illusts 返回数据异常（可能无公开作品或 user_id 错误）body=" + clip);
    }
}

let count = 0;
let author = "";
for item in illusts {
    let id = json_path(item, "id");
    if type_of(id) != "i64" { continue; }
    let title = str_safe(json_path(item, "title"));
    if len(title) == 0 { title = "Untitled"; }
    let caption = str_safe(json_path(item, "caption"));
    let item_author = str_safe(json_path(item, "user.name"));
    if len(item_author) > 0 && len(author) == 0 {
        author = item_author;
    }
    let published = iso_to_unix(str_safe(json_path(item, "create_date")));

    set_field("guid", "pixiv-" + str_safe(id));
    set_field("title", title);
    set_field("url", "https://www.pixiv.net/artworks/" + str_safe(id));
    set_field("author", item_author);
    if published > 0 { set_field("published_at", published); }
    let html = "";
    let pages = json_path(item, "meta_pages");
    if type_of(pages) == "array" && len(pages) > 0 {
        for page in pages {
            let img = str_safe(json_path(page, "image_urls.medium"));
            let original = str_safe(json_path(page, "image_urls.original"));
            if len(original) > 0 {
                html += "<p><img src=\"" + original + "\"></p>";
            } else if len(img) > 0 {
                html += "<p><img src=\"" + img + "\"></p>";
            }
        }
    } else {
        let img = str_safe(json_path(item, "image_urls.medium"));
        if len(img) == 0 {
            img = str_safe(json_path(item, "image_urls.square_medium"));
        }
        let original = str_safe(json_path(item, "meta_single_page.original_image_url"));
        if len(original) > 0 {
            html += "<p><img src=\"" + original + "\"></p>";
        } else if len(img) > 0 {
            html += "<p><img src=\"" + img + "\"></p>";
        }
    }
    if len(caption) > 0 {
        html += "<div>" + caption + "</div>";
    }
    set_field("content_html", html);
    add_entry();
    count += 1;
}
log("info", "pixiv 收集 " + count + " 条");
if len(author) > 0 {
    set_feed_title("Pixiv " + author);
}
if count == 0 {
    log("warn", "pixiv user_id=" + mid + " 返回 0 条（可能无公开插画）");
}
