// Bilibili 用户投稿 Tier 2 适配器。
//
// 算法：b_nut(客户端生成) + buvid3/buvid4(SPI 匿名拿) → nav(wbi keys) →
//       wbi 签名 → wbi/arc/search 拉视频列表。
//
// SOURCE_URL 由 Host 通过 Rhai 全局常量注入（见 runtime.rs::run_script），
// 形如 "https://space.bilibili.com/12345"。
//
// 匿名模式：不需要用户凭证。buvid3 是设备指纹，匿名可从 SPI endpoint 拿。
// SESSDATA 登录态 (高清 / 私密列表) 留待 M7+ 接入 CredentialStore。

const MIXIN_KEY_ENC_TAB = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49,
    33, 9, 42, 19, 29, 28, 14, 39, 12, 38, 41, 13, 37, 36, 25, 51, 0, 4, 44, 52,
    6, 21, 54, 16, 26, 11, 22, 40, 7, 30, 55, 48, 24, 1, 20, 57, 34, 17, 59, 61,
    56, 60, 63, 62,
];

const UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const REFERER = "https://www.bilibili.com/";

// ---- wbi 签名算法 (https://github.com/SocialSisterYi/bilibili-API-collect) ----
//
// Rhai 限制：函数不能访问外部 const/let 变量（global module 对数组不生效），
// 也不能嵌套定义 fn。因此 MIXIN_KEY_ENC_TAB 作为 wbi_sign 的局部变量，
// filter_chars / to_str 放全局。

fn filter_chars(s) {
    let out = "";
    for c in s {
        if c != '!' && c != "'" && c != '(' && c != ')' && c != '*' {
            out += c;
        }
    }
    return out;
}

fn to_str(v) {
    let t = type_of(v);
    if t == "i64" || t == "i32" || t == "int" {
        return "" + v;
    }
    return v;
}

/// 把 code 字段转为可读字符串；缺失（响应非 JSON）时显示 "?"。
fn code_str(code) {
    if type_of(code) == "i64" {
        return "" + code;
    }
    return "?";
}

/// 计算 wbi 签名后的参数 map (含 wts + w_rid)。
fn wbi_sign(params, img_key, sub_key, wts) {
    let MIXIN_KEY_ENC_TAB = [
        46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49,
        33, 9, 42, 19, 29, 28, 14, 39, 12, 38, 41, 13, 37, 36, 25, 51, 0, 4, 44, 52,
        6, 21, 54, 16, 26, 11, 22, 40, 7, 30, 55, 48, 24, 1, 20, 57, 34, 17, 59, 61,
        56, 60, 63, 62,
    ];
    let orig = img_key + sub_key;
    let mixin_key = "";
    for i in MIXIN_KEY_ENC_TAB {
        if i >= len(orig) { break; }
        mixin_key += orig[i];
        if len(mixin_key) >= 32 { break; }
    }

    params["wts"] = wts;
    let keys = params.keys();
    keys.sort();
    let query = "";
    for k in keys {
        let v_str = to_str(params[k]);
        let filtered = filter_chars(v_str);
        if len(query) > 0 { query += "&"; }
        query += k + "=" + filtered;
    }
    let w_rid = md5(query + mixin_key);
    params["w_rid"] = w_rid;
    return params;
}

fn build_query(params) {
    let keys = params.keys();
    keys.sort();
    let query = "";
    let first = true;
    for k in keys {
        if !first { query += "&"; }
        query += k + "=" + to_str(params[k]);
        first = false;
    }
    return query;
}

/// 拉取 arc/search 并重试（-352 风控是概率性的，间隔重试可显著提高成功率）。
/// 最终仍失败时返回最后一次响应（由调用方 throw 报错，让 UI 可见失败原因）。
fn fetch_arc_with_retry(url, hdrs) {
    let tries = 5;
    let attempt = 0;
    let arc = parse_json("{}");
    while attempt < tries {
        let resp = http_get(url, hdrs, "");
        arc = parse_json(resp["body"]);
        let code = json_path(arc, "code");
        if type_of(code) == "i64" && code == 0 {
            return arc;
        }
        log("warn", "bilibili arc/search 第 " + attempt + " 次失败 code=" + code_str(code));
        attempt += 1;
        if attempt < tries {
            sleep_ms(2000);
        }
    }
    return arc;
}

// ---- URL 解析：从 space.bilibili.com/<mid>[/...] 提取 mid ----

fn extract_mid(url) {
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

// ---- 主流程 ----

let mid = extract_mid(SOURCE_URL);
if len(mid) == 0 {
    log("error", "bilibili: 无法从 SOURCE_URL 提取 mid: " + SOURCE_URL);
} else {
    log("info", "bilibili mid=" + mid);

    let b_nut = now();
    let cookie_base = "b_nut=" + b_nut;
    let hdrs = #{
        "User-Agent": UA,
        "Referer": REFERER,
        "Cookie": cookie_base,
    };

    // 1. SPI 拿 buvid3/buvid4
    let spi_resp = http_get(
        "https://api.bilibili.com/x/frontend/finger/spi",
        hdrs,
        ""
    );
    if spi_resp["status"] != 200 {
        log("error", "bilibili SPI HTTP " + spi_resp["status"]);
    } else {
        let spi = parse_json(spi_resp["body"]);
        let buvid3 = json_path(spi, "data.b_3");
        let buvid4 = json_path(spi, "data.b_4");
        let cookie = cookie_base + "; buvid3=" + to_str(buvid3) + "; buvid4=" + to_str(buvid4);

        hdrs["Cookie"] = cookie;

        // 2. nav 拿 wbi keys (即使 code=-101 未登录也会返回 wbi_img)
        let nav_resp = http_get(
            "https://api.bilibili.com/x/web-interface/nav",
            hdrs,
            ""
        );
        let nav = parse_json(nav_resp["body"]);
        let img_url = to_str(json_path(nav, "data.wbi_img.img_url"));
        let sub_url = to_str(json_path(nav, "data.wbi_img.sub_url"));
        let img_key = img_url.split("/")[len(img_url.split("/")) - 1].split(".")[0];
        let sub_key = sub_url.split("/")[len(sub_url.split("/")) - 1].split(".")[0];
        log("info", "bilibili wbi keys: img=" + img_key + " sub=" + sub_key);

        // 2.5 acc/info 拿 UP 主昵称 → 设置订阅名称（如 "Bilibili 某UP主"）。
        // 注意：acc/info 在部分网络（如数据中心 IP 风控）下可能失败，
        // 失败时静默回退到 manifest.name，不中断主流程。
        let acc_url = "https://api.bilibili.com/x/space/wbi/acc/info?mid=" + mid
            + "&" + build_query(wbi_sign(#{}, img_key, sub_key, now()));
        let acc_resp = http_get(acc_url, hdrs, "");
        let acc = parse_json(acc_resp["body"]);
        let acc_code = json_path(acc, "code");
        if type_of(acc_code) == "i64" {
            log("info", "bilibili acc/info code=" + acc_code);
        }
        let uname = to_str(json_path(acc, "data.name"));
        if type_of(uname) == "string" && len(uname) > 0 {
            set_feed_title("Bilibili " + uname);
            log("info", "bilibili UP主: " + uname);
        } else {
            log("warn", "bilibili acc/info 未返回昵称（跳过订阅名设置）");
        }

        // 3. wbi 签名 + 拉取视频列表（带重试）
        let params = #{
            "mid": mid,
            "pn": "1",
            "ps": "30",
        };
        let signed = wbi_sign(params, img_key, sub_key, now());
        let url = "https://api.bilibili.com/x/space/wbi/arc/search?" + build_query(signed);

        let arc = fetch_arc_with_retry(url, hdrs);
        let code = json_path(arc, "code");
        if to_str(code) != "0" {
            let msg = to_str(json_path(arc, "message"));
            log("error", "bilibili arc/search code=" + code_str(code) + " msg=" + msg);
            // throw 让 refresh 走 Error 分支，订阅/刷新失败原因会显示在 UI。
            // -352 风控与网络环境相关，提示用户可尝试给该订阅开启「使用代理」。
            throw("bilibili arc/search 失败 code=" + code_str(code) + " msg=" + msg
                + "（若持续失败，请给该订阅开启「使用代理」）");
        } else {
            let vlist = json_path(arc, "data.list.vlist");
            let count = 0;
            for item in vlist {
                let bvid = to_str(json_path(item, "bvid"));
                let title = to_str(json_path(item, "title"));
                let desc = to_str(json_path(item, "description"));
                let pic = to_str(json_path(item, "pic"));
                let author = to_str(json_path(item, "author"));
                let created = json_path(item, "created");
                let length = to_str(json_path(item, "length"));

                set_field("guid", bvid);
                set_field("title", title);
                set_field("url", "https://www.bilibili.com/video/" + bvid);
                set_field("author", author);
                set_field("published_at", created);
                set_field("summary", desc);
                set_field("content_html",
                    "<p>" + desc + "</p>" +
                    "<img src=\"" + pic + "\">" +
                    "<p>时长: " + length + "</p>");
                add_entry();
                count += 1;
            }
            log("info", "bilibili 收集 " + count + " 条");
            if count == 0 {
                log("warn", "bilibili mid=" + mid + " 返回 0 条（可能被风控或该 UP 主无公开投稿）");
            }
        }
    }
}
