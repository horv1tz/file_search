//! Оформление: шрифты, цвета, светлая и тёмная темы.

use std::sync::Arc;

use egui::{Color32, Context, FontData, FontDefinitions, FontFamily, Theme as EguiTheme};

use crate::settings::Theme;

/// Типы файлов для быстрых фильтров: (название, расширения).
pub const GROUPS: [(&str, &[&str]); 5] = [
    ("Word", &["doc", "docx", "docm", "dot", "dotx", "dotm", "odt", "rtf"]),
    ("Excel", &["xls", "xlsx", "xlsm", "xlsb", "xlt", "xltx", "xltm", "ods", "csv"]),
    ("PowerPoint", &["ppt", "pptx", "pptm", "pps", "ppsx", "pot", "potx", "odp"]),
    ("PDF", &["pdf"]),
    ("Текст", &["txt", "md", "log", "json", "xml", "html", "htm", "yaml", "yml", "ini", "cfg"]),
];

pub struct Palette {
    pub card: Color32,
    pub card_hover: Color32,
    pub card_selected: Color32,
    pub line: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub mark_bg: Color32,
    pub mark_fg: Color32,
    pub warn: Color32,
    pub ok: Color32,
    pub danger: Color32,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            card: Color32::from_rgb(0x1a, 0x1d, 0x23),
            card_hover: Color32::from_rgb(0x20, 0x24, 0x2c),
            card_selected: Color32::from_rgb(0x1b, 0x27, 0x40),
            line: Color32::from_rgb(0x2c, 0x31, 0x3a),
            muted: Color32::from_rgb(0x9a, 0xa3, 0xb2),
            accent: Color32::from_rgb(0x7a, 0xa2, 0xff),
            mark_bg: Color32::from_rgb(0x5c, 0x4a, 0x00),
            mark_fg: Color32::from_rgb(0xff, 0xf3, 0xc4),
            warn: Color32::from_rgb(0xf5, 0x9e, 0x6b),
            ok: Color32::from_rgb(0x5c, 0xc9, 0x8d),
            danger: Color32::from_rgb(0xf2, 0x8b, 0x8b),
        }
    } else {
        Palette {
            card: Color32::WHITE,
            card_hover: Color32::from_rgb(0xf4, 0xf7, 0xfd),
            card_selected: Color32::from_rgb(0xe8, 0xf0, 0xff),
            line: Color32::from_rgb(0xe0, 0xe4, 0xea),
            muted: Color32::from_rgb(0x66, 0x70, 0x85),
            accent: Color32::from_rgb(0x2f, 0x6f, 0xed),
            mark_bg: Color32::from_rgb(0xff, 0xf0, 0xa8),
            mark_fg: Color32::from_rgb(0x3a, 0x2e, 0x00),
            warn: Color32::from_rgb(0xc2, 0x41, 0x0c),
            ok: Color32::from_rgb(0x1e, 0x71, 0x45),
            danger: Color32::from_rgb(0xb9, 0x1c, 0x1c),
        }
    }
}

/// Цвет и подпись значка типа файла.
pub fn badge(ext: &str, dark: bool) -> (Color32, String) {
    let pick = |light: [u8; 3], dk: [u8; 3]| {
        let c = if dark { dk } else { light };
        Color32::from_rgb(c[0], c[1], c[2])
    };
    let color = match GROUPS.iter().position(|(_, exts)| exts.contains(&ext)) {
        Some(0) => pick([0x2b, 0x57, 0x9a], [0x4f, 0x7d, 0xc4]),
        Some(1) => pick([0x1e, 0x71, 0x45], [0x2f, 0x93, 0x5e]),
        Some(2) => pick([0xc2, 0x41, 0x0c], [0xd9, 0x6a, 0x35]),
        Some(3) => pick([0xb9, 0x1c, 0x1c], [0xd0, 0x4a, 0x4a]),
        Some(_) => pick([0x55, 0x60, 0x75], [0x6b, 0x76, 0x8b]),
        None => pick([0x66, 0x70, 0x85], [0x6b, 0x76, 0x8b]),
    };
    let label = if ext.is_empty() { "ФАЙЛ".to_string() } else { ext.to_uppercase() };
    (color, label)
}

/// Подключает системный шрифт Windows (полный набор символов и привычный вид),
/// встроенные шрифты egui остаются запасными.
pub fn install_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    // Windows может стоять не на диске C: — берём папку из переменной окружения.
    let windir = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot"));
    let mut candidates = Vec::new();
    if let Some(windir) = windir {
        candidates.push(std::path::PathBuf::from(windir).join("Fonts").join("segoeui.ttf"));
    }
    candidates.push(std::path::PathBuf::from(r"C:\Windows\Fonts\segoeui.ttf"));
    candidates.push(std::path::PathBuf::from("/System/Library/Fonts/Supplemental/Arial.ttf"));
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert("system-ui".into(), Arc::new(FontData::from_owned(bytes)));
            fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "system-ui".into());
            break;
        }
    }
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &Context, theme: Theme) {
    ctx.set_theme(match theme {
        Theme::Auto => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark => egui::ThemePreference::Dark,
    });
    for (egui_theme, dark) in [(EguiTheme::Light, false), (EguiTheme::Dark, true)] {
        let p = palette(dark);
        ctx.style_mut_of(egui_theme, |style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
            // Выделяемые надписи перехватывали щелчки: по карточке результата не срабатывали двойной щелчок и меню.
            style.interaction.selectable_labels = false;
            let v = &mut style.visuals;
            v.selection.bg_fill = p.accent.gamma_multiply(if dark { 0.45 } else { 0.25 });
            v.selection.stroke = egui::Stroke::new(1.0, p.accent);
            v.hyperlink_color = p.accent;
            let radius = egui::CornerRadius::same(7);
            for w in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.corner_radius = radius;
            }
            v.window_corner_radius = egui::CornerRadius::same(12);
            // Основной текст контрастнее, чем по умолчанию в egui (там он приглушённо-серый).
            let (body, strong) = if dark {
                (Color32::from_rgb(0xd2, 0xd7, 0xe0), Color32::from_rgb(0xee, 0xf1, 0xf6))
            } else {
                (Color32::from_rgb(0x2a, 0x2f, 0x3a), Color32::from_rgb(0x12, 0x15, 0x1b))
            };
            v.widgets.noninteractive.fg_stroke.color = body;
            v.widgets.inactive.fg_stroke.color = body;
            v.widgets.hovered.fg_stroke.color = strong;
            v.widgets.active.fg_stroke.color = strong;
            if !dark {
                v.panel_fill = Color32::from_rgb(0xf6, 0xf7, 0xf9);
                v.extreme_bg_color = Color32::WHITE;
            } else {
                v.panel_fill = Color32::from_rgb(0x10, 0x12, 0x16);
                v.extreme_bg_color = Color32::from_rgb(0x16, 0x19, 0x1e);
            }
        });
    }
}
