//! Install proportional fonts with CJK coverage for the egui shell.
//! WebView uses system fonts separately; this only affects chrome UI.

use eframe::egui;
use std::sync::Arc;

/// Prefer Windows system CJK fonts; fall back to egui default (Latin only).
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    if let Some((name, data)) = load_cjk_font() {
        fonts
            .font_data
            .insert(name.clone(), Arc::new(egui::FontData::from_owned(data)));

        // Prepend so CJK glyphs resolve; Latin still falls through family list as needed.
        for family in [
            egui::FontFamily::Proportional,
            egui::FontFamily::Monospace,
        ] {
            if let Some(list) = fonts.families.get_mut(&family) {
                list.insert(0, name.clone());
            }
        }

        ctx.set_fonts(fonts);
        eprintln!("glean-spike: loaded UI font `{name}`");
    } else {
        eprintln!(
            "glean-spike: no CJK system font found; shell Chinese will show as tofu (□). \
             WebView body is unaffected."
        );
    }
}

fn load_cjk_font() -> Option<(String, Vec<u8>)> {
    // Prefer single-face TTF first: some egui/ab_glyph builds are picky about .ttc.
    const CANDIDATES: &[(&str, &str)] = &[
        ("SimHei", r"C:\Windows\Fonts\simhei.ttf"),
        ("Microsoft YaHei", r"C:\Windows\Fonts\msyh.ttf"),
        ("Microsoft YaHei", r"C:\Windows\Fonts\msyh.ttc"),
        ("SimSun", r"C:\Windows\Fonts\simsun.ttc"),
        ("Noto Sans CJK SC", r"C:\Windows\Fonts\NotoSansCJKsc-Regular.otf"),
        ("Segoe UI", r"C:\Windows\Fonts\segoeui.ttf"),
    ];

    for (name, path) in CANDIDATES {
        match std::fs::read(path) {
            Ok(data) if data.len() > 1000 => return Some(((*name).to_string(), data)),
            _ => continue,
        }
    }
    None
}
