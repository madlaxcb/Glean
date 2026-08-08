// Civitai 创作者订阅适配器（Tier 2 脚本）。
//
// 支持 URL：
//   https://civitai.com/user/{username}          → 模型+图片+文章
//   https://civitai.red/user/{username}          → 同上（含 NSFW）
//   https://civitai.{com,red}/user/{username}/models   → 仅模型
//   https://civitai.{com,red}/user/{username}/images   → 仅图片
//   https://civitai.{com,red}/user/{username}/videos   → 仅视频
//   https://civitai.{com,red}/user/{username}/articles → 仅文章
//
// 两个域名共享同一套 API 路径：
//   civitai.com = SFW
//   civitai.red = SFW + NSFW 全量
//
// 数据获取方式：
//   1. 模型：Public REST API
//      GET {base}/api/v1/models?username={user}&sort=Newest&limit=20
//   2. 图片/视频：Public REST API
//      GET {base}/api/v1/images?username={user}&sort=Newest&limit=100
//      （视频是 type="video" 的 image 记录，从 URL 扩展名判断）
//   3. 文章：tRPC（公开端点，无需认证）
//      GET {base}/api/trpc/article.getInfinite?input={url_encoded_json}
//
// 认证：可选 API Key（civitai_api_key），通过 Authorization: Bearer 注入。
// 匿名访问可用，但限速更低。

const UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const ADAPTER_VERSION = "20260808-1";

// ---- 工具函数 ----

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

/// 判断 URL 是否是视频文件。
fn is_video_url(url) {
    return url.contains(".mp4") || url.contains(".MP4") || url.contains(".Mp4")
        || url.contains(".webm") || url.contains(".WebM") || url.contains(".WEBM")
        || url.contains("/video/");
}

fn civitai_image_url(url, width) {
    let result = url;
    result.replace("original=true", "width=" + width);
    return result;
}

// ---- URL 解析 ----
// 域名优先级：URL 自带域名 > 插件设置中的 domain > 默认 civitai.com

let parts = SOURCE_URL.split("/");
let url_host = "";
if len(parts) >= 3 {
    url_host = parts[2];
}

// 从插件设置读取域名（用户在设置页面选择）。
let config_domain = "civitai.com";
if len(PLUGIN_CONFIG) > 0 {
    try {
        let cfg = parse_json(PLUGIN_CONFIG);
        let d = str_safe(json_path(cfg, "domain"));
        if len(d) > 0 { config_domain = d; }
    } catch {}
}

// URL 中的域名优先（用户订阅 .red 就走 .red），
// URL 不含 civitai 域名时用设置中的域名。
let base = "";
if url_host.contains("civitai") {
    base = "https://" + url_host;
} else {
    base = "https://" + config_domain;
}

let username = "";
let section = "";

let i = 0;
while i < len(parts) {
    if parts[i] == "user" && i + 1 < len(parts) {
        username = parts[i + 1];
        if i + 2 < len(parts) {
            section = parts[i + 2];
        }
        break;
    }
    i += 1;
}

if len(username) == 0 {
    throw("civitai: 无法从 URL 提取用户名: " + SOURCE_URL);
}

log("info", "civitai base=" + base + " user=" + username + " section=" + section);

// ---- 内容范围 ----

let do_models = section == "" || section == "posts" || section == "models";
let do_images = section == "" || section == "posts" || section == "images";
let do_videos = section == "" || section == "posts" || section == "videos";
let do_articles = section == "" || section == "posts" || section == "articles";

// ---- 请求头 ----

let hdrs = #{
    "User-Agent": UA,
    "Accept": "application/json",
    "Referer": base + "/",
};

// civitai.red 含 NSFW 内容，REST API 默认过滤，需显式传 nsfw=true。
// civitai.com 仅 SFW，nsfw 参数无副作用但保持一致。
let nsfw_param = "&nsfw=true";

// ---- 状态 ----

let existing = EXISTING_GUIDS;
let count = 0;
log("info", "civitai adapter_version=" + ADAPTER_VERSION);

// ---- 拉取模型 (REST API) ----

if do_models {
    let url = base + "/api/v1/models?username=" + username + "&sort=Newest&limit=20" + nsfw_param;
    log("info", "civitai 请求模型: " + url);
    let resp = http_get(url, hdrs, "civitai_api_key");
    log("info", "civitai 模型响应 status=" + str_safe(resp["status"]) + " body_len=" + len(str_safe(resp["body"])));
    if resp["status"] == 200 {
        try {
            let data = parse_json(str_safe(resp["body"]));
            let items = json_path(data, "items");
            if type_of(items) == "array" {
                let model_count = 0;
                for item in items {
                    let id = str_safe(json_path(item, "id"));
                    if len(id) == 0 { continue; }
                    let guid = "civitai-model-" + id;

                    let name = str_safe(json_path(item, "name"));
                    let desc = str_safe(json_path(item, "description"));
                    let mtype = str_safe(json_path(item, "type"));
                    let dl_count = str_safe(json_path(item, "stats.downloadCount"));

                    // 封面图：第一个版本的第一个图片。
                    let thumb = "";
                    let published = 0;
                    let versions = json_path(item, "modelVersions");
                    if type_of(versions) == "array" && len(versions) > 0 {
                        let imgs = json_path(versions[0], "images");
                        if type_of(imgs) == "array" && len(imgs) > 0 {
                            thumb = str_safe(json_path(imgs[0], "url"));
                        }
                        published = iso_to_unix(str_safe(json_path(versions[0], "createdAt")));
                    }

                    let html = "";
                    if len(desc) > 0 { html += "<p>" + html_escape(desc) + "</p>"; }
                    if len(thumb) > 0 { html += "<p><img src=\"" + html_escape(thumb) + "\"></p>"; }
                    html += "<p>类型: " + html_escape(mtype);
                    if len(dl_count) > 0 && dl_count != "0" { html += " | 下载: " + dl_count; }
                    html += "</p>";

                    if len(name) == 0 { name = "模型 #" + id; }

                    set_field("guid", guid);
                    set_field("title", name);
                    set_field("url", base + "/models/" + id);
                    set_field("author", username);
                    if published > 0 { set_field("published_at", published); }
                    if len(thumb) > 0 { set_field("thumbnail", thumb); }
                    set_field("content_html", html);
                    add_entry();
                    model_count += 1;
                    count += 1;
                }
                log("info", "civitai 模型 +" + model_count);
            }
        } catch(err) {
            log("warn", "civitai 模型解析失败: " + str_safe(err));
        }
    } else {
        log("warn", "civitai 模型 HTTP " + resp["status"]);
    }
}

// ---- 拉取图片/视频 (REST API) ----

if do_images || do_videos {
    let url = base + "/api/v1/images?username=" + username + "&sort=Newest&limit=100" + nsfw_param;
    log("info", "civitai 请求图片/视频: " + url);
    let resp = http_get(url, hdrs, "civitai_api_key");
    log("info", "civitai 图片响应 status=" + str_safe(resp["status"]) + " body_len=" + len(str_safe(resp["body"])));
    if resp["status"] == 200 {
        try {
            let data = parse_json(str_safe(resp["body"]));
            let items = json_path(data, "items");
            if type_of(items) == "array" {
                let img_count = 0;
                let vid_count = 0;
                for item in items {
                    let img_url = str_safe(json_path(item, "url"));
                    let id = str_safe(json_path(item, "id"));
                    if len(img_url) == 0 || len(id) == 0 { continue; }

                    let video = is_video_url(img_url);
                    let content_url = if video { img_url } else { civitai_image_url(img_url, "1200") };
                    let thumbnail_url = if video { "" } else { civitai_image_url(img_url, "450") };
                    if video && !do_videos { continue; }
                    if !video && !do_images { continue; }

                    let guid = if video { "civitai-video-" + id } else { "civitai-image-" + id };

                    let width = str_safe(json_path(item, "width"));
                    let height = str_safe(json_path(item, "height"));

                    let html = "";
                    if video {
                        html = "<p><video controls src=\"" + html_escape(content_url) + "\"></video></p>";
                    } else {
                        html = "<p><img src=\"" + html_escape(content_url) + "\"></p>";
                    }
                    if len(width) > 0 && len(height) > 0 && width != "0" {
                        html += "<p>" + width + " x " + height + "</p>";
                    }

                    let title = if video { "视频 #" + id } else { "图片 #" + id };

                    set_field("guid", guid);
                    set_field("title", title);
                    set_field("url", base + "/images/" + id);
                    set_field("author", username);
                    // 缩略图：图片直接用自身 URL，视频用 poster 帧或首帧。
                    if video {
                        let poster = str_safe(json_path(item, "thumbnailUrl"));
                        if len(poster) > 0 {
                            set_field("thumbnail", poster);
                        }
                    } else {
                        set_field("thumbnail", thumbnail_url);
                    }
                    set_field("content_html", html);
                    add_entry();
                    if video { vid_count += 1; } else { img_count += 1; }
                    count += 1;
                }
                log("info", "civitai 图片 +" + img_count + " 视频 +" + vid_count);
            }
        } catch(err) {
            log("warn", "civitai 图片解析失败: " + str_safe(err));
        }
    } else {
        log("warn", "civitai 图片/视频 HTTP " + resp["status"]);
    }
}

// ---- 拉取文章 (tRPC) ----

if do_articles {
    let inner = "{\"username\":\"" + username + "\",\"sort\":\"Newest\",\"limit\":20,\"period\":\"AllTime\"}";
    let input = url_encode("{\"json\":" + inner + "}");
    let url = base + "/api/trpc/article.getInfinite?input=" + input;
    log("info", "civitai 请求文章: " + url);
    let resp = http_get(url, hdrs, "civitai_api_key");
    log("info", "civitai 文章响应 status=" + str_safe(resp["status"]) + " body_len=" + len(str_safe(resp["body"])));
    if resp["status"] == 200 {
        try {
            let data = parse_json(str_safe(resp["body"]));
            // tRPC 响应格式：result.data.json.items
            let items = json_path(data, "result.data.json.items");
            if type_of(items) != "array" {
                items = json_path(data, "result.data.json");
            }
            if type_of(items) == "array" {
                let art_count = 0;
                for item in items {
                    let id = str_safe(json_path(item, "id"));
                    if len(id) == 0 { continue; }
                    let guid = "civitai-article-" + id;

                    let title = str_safe(json_path(item, "title"));
                    let content = str_safe(json_path(item, "content"));
                    let published = iso_to_unix(str_safe(json_path(item, "publishedAt")));

                    if len(title) == 0 { title = "文章 #" + id; }
                    if len(content) == 0 { content = str_safe(json_path(item, "excerpt")); }

                    set_field("guid", guid);
                    set_field("title", title);
                    set_field("url", base + "/articles/" + id);
                    set_field("author", username);
                    if published > 0 { set_field("published_at", published); }
                    set_field("content_html", content);
                    add_entry();
                    art_count += 1;
                    count += 1;
                }
                log("info", "civitai 文章 +" + art_count);
            }
        } catch(err) {
            log("warn", "civitai 文章解析失败: " + str_safe(err));
        }
    } else {
        log("warn", "civitai 文章 HTTP " + resp["status"]);
    }
}

// ---- 完成 ----

set_feed_title("Civitai " + username);
log("info", "civitai user=" + username + " 收集 " + count + " 条");
