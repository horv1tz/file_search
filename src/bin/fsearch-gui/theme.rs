//! Дизайн-система: палитра светлой и тёмной тем, шкалы шрифтов, скруглений и отступов, общие элементы.
//!
//! Правила: числа размеров и цвета в остальном коде не пишутся — только константы и функции отсюда.
//! Все цвета подобраны так, чтобы текст читался с контрастом не ниже 4,5:1 (проверяет тест внизу файла).

use std::sync::Arc;

use egui::{
    Align, Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Layout, Response, RichText,
    Sense, Stroke, TextStyle, Theme as EguiTheme, Ui, vec2,
};

use crate::settings::Theme;

/// Типы файлов для быстрых фильтров: (название, расширения).
pub const GROUPS: [(&str, &[&str]); 5] = [
    ("Word", &["doc", "docx", "docm", "dot", "dotx", "dotm", "odt", "rtf"]),
    ("Excel", &["xls", "xlsx", "xlsm", "xlsb", "xlt", "xltx", "xltm", "ods", "csv"]),
    ("PowerPoint", &["ppt", "pptx", "pptm", "pps", "ppsx", "pot", "potx", "odp"]),
    ("PDF", &["pdf"]),
    ("Текст", &["txt", "md", "log", "json", "xml", "html", "htm", "yaml", "yml", "ini", "cfg"]),
];

// ------------------------------------------------------------------ шкалы

/// Размеры шрифта: заголовок, подзаголовок, основной, мелкий, подпись.
pub const FONT_TITLE: f32 = 20.0;
pub const FONT_HEADING: f32 = 16.0;
pub const FONT_BODY: f32 = 14.0;
pub const FONT_SMALL: f32 = 13.0;
pub const FONT_CAPTION: f32 = 12.0;
/// Моноширинный (просмотр текста, примеры запросов).
pub const FONT_MONO: f32 = 13.0;

/// Скругления: элементы управления, карточки и панели, «таблетки» и поле поиска.
pub const RADIUS_SM: u8 = 8;
pub const RADIUS_MD: u8 = 12;
pub const RADIUS_LG: u8 = 16;

/// Отступы.
pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 12.0;
pub const SPACE_LG: f32 = 16.0;
pub const SPACE_XL: f32 = 24.0;

/// Высота всех элементов управления (кнопки, выпадающие списки, «таблетки») и минимальная цель нажатия.
pub const CONTROL_H: f32 = 32.0;
/// Высота строки поиска.
pub const SEARCH_H: f32 = 48.0;
/// Толщина единственной линии-разделителя.
pub const LINE_W: f32 = 1.0;

pub fn radius(r: u8) -> CornerRadius {
    CornerRadius::same(r)
}

// ------------------------------------------------------------------ палитра

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// Фон окна.
    pub bg: Color32,
    /// Поверхность первого уровня: карточки, панели.
    pub surface: Color32,
    /// Поверхность второго уровня: поля, кнопки, вложенные блоки.
    pub surface_alt: Color32,
    pub hover: Color32,
    pub selected: Color32,
    /// Единственная линия-разделитель и рамки.
    pub line: Color32,
    pub text: Color32,
    pub text_2: Color32,
    /// Приглушённый текст (пути, даты, подписи).
    pub muted: Color32,
    pub accent: Color32,
    /// Цвет текста на заливке цветом акцента.
    pub on_accent: Color32,
    pub ok: Color32,
    pub ok_soft: Color32,
    pub warn: Color32,
    pub warn_soft: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
    /// Красная кнопка опасного действия и цвет текста на ней.
    pub danger_solid: Color32,
    pub on_danger: Color32,
    pub mark_bg: Color32,
    pub mark_fg: Color32,
    /// Редактор просмотра текста: номера строк и строка с выбранным совпадением.
    pub gutter: Color32,
    pub line_hl: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub const LIGHT: Palette = Palette {
    dark: false,
    bg: rgb(0xf5f6f8),
    surface: rgb(0xffffff),
    surface_alt: rgb(0xeef0f4),
    hover: rgb(0xf3f6fc),
    selected: rgb(0xe5edff),
    line: rgb(0xdde1e8),
    text: rgb(0x1d222c),
    text_2: rgb(0x434b5a),
    muted: rgb(0x5b6475),
    accent: rgb(0x1f55d0),
    on_accent: rgb(0xffffff),
    ok: rgb(0x17663b),
    ok_soft: rgb(0xe3f3ea),
    warn: rgb(0x9a3c08),
    warn_soft: rgb(0xfdeee2),
    danger: rgb(0xb3261e),
    danger_soft: rgb(0xfce8e6),
    danger_solid: rgb(0xb3261e),
    on_danger: rgb(0xffffff),
    mark_bg: rgb(0xfff0a8),
    mark_fg: rgb(0x3a2e00),
    gutter: rgb(0x8a93a5),
    line_hl: rgb(0xeaf0fc),
};

pub const DARK: Palette = Palette {
    dark: true,
    bg: rgb(0x11131a),
    surface: rgb(0x191c24),
    surface_alt: rgb(0x232733),
    hover: rgb(0x1f232d),
    selected: rgb(0x1c2b4d),
    line: rgb(0x2c313d),
    text: rgb(0xe8ebf1),
    text_2: rgb(0xc5cbd6),
    muted: rgb(0x9aa3b4),
    accent: rgb(0x8fb0ff),
    on_accent: rgb(0x0b1530),
    ok: rgb(0x62d49c),
    ok_soft: rgb(0x15281f),
    warn: rgb(0xf2a672),
    warn_soft: rgb(0x2d2018),
    danger: rgb(0xff9494),
    danger_soft: rgb(0x2e1b1d),
    danger_solid: rgb(0xe5645f),
    on_danger: rgb(0x2a0a0a),
    mark_bg: rgb(0x5c4a00),
    mark_fg: rgb(0xfff3c4),
    gutter: rgb(0x6d7689),
    line_hl: rgb(0x242d44),
};

pub fn palette(dark: bool) -> Palette {
    if dark { DARK } else { LIGHT }
}

/// Цвет плитки типа файла; белый текст на ней читается в обеих темах.
pub fn badge(ext: &str, dark: bool) -> (Color32, String) {
    let pick = |light: u32, dk: u32| rgb(if dark { dk } else { light });
    let color = match GROUPS.iter().position(|(_, exts)| exts.contains(&ext)) {
        Some(0) => pick(0x2b579a, 0x3b68bd),
        Some(1) => pick(0x1e7145, 0x267f50),
        Some(2) => pick(0xb4400d, 0xb8501d),
        Some(3) => pick(0xb91c1c, 0xc94242),
        Some(_) => pick(0x556075, 0x66718a),
        None => pick(0x667085, 0x66718a),
    };
    let label = if ext.is_empty() { "ФАЙЛ".to_string() } else { ext.to_uppercase() };
    (color, label)
}

// ------------------------------------------------------------------ шрифты и текст

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn title(pal: &Palette, text: impl Into<String>) -> RichText {
    RichText::new(text).size(FONT_TITLE).color(pal.text)
}

pub fn heading(pal: &Palette, text: impl Into<String>) -> RichText {
    RichText::new(text).size(FONT_HEADING).color(pal.text)
}

pub fn body(pal: &Palette, text: impl Into<String>) -> RichText {
    RichText::new(text).size(FONT_BODY).color(pal.text)
}

/// Мелкий второстепенный текст (пояснения).
pub fn small(pal: &Palette, text: impl Into<String>) -> RichText {
    RichText::new(text).size(FONT_SMALL).color(pal.text_2)
}

/// Подпись: пути, даты, названия полей.
pub fn caption(pal: &Palette, text: impl Into<String>) -> RichText {
    RichText::new(text).size(FONT_CAPTION).color(pal.muted)
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

/// Применяет дизайн-систему к egui (один раз при запуске и при смене темы).
pub fn apply(ctx: &Context, theme: Theme) {
    ctx.set_theme(match theme {
        Theme::Auto => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark => egui::ThemePreference::Dark,
    });
    for (egui_theme, dark) in [(EguiTheme::Light, false), (EguiTheme::Dark, true)] {
        let p = palette(dark);
        ctx.style_mut_of(egui_theme, |style| {
            style.text_styles = [
                (TextStyle::Heading, font(FONT_TITLE)),
                (TextStyle::Body, font(FONT_BODY)),
                (TextStyle::Button, font(FONT_BODY)),
                (TextStyle::Small, font(FONT_CAPTION)),
                (TextStyle::Monospace, FontId::monospace(FONT_MONO)),
            ]
            .into();
            style.spacing.item_spacing = vec2(SPACE_SM, SPACE_SM);
            style.spacing.button_padding = vec2(SPACE_MD, SPACE_XS);
            style.spacing.interact_size = vec2(CONTROL_H, CONTROL_H);
            style.spacing.combo_height = 320.0;
            style.spacing.menu_margin = egui::Margin::same(SPACE_XS as i8);
            style.spacing.window_margin = egui::Margin::same(SPACE_LG as i8);
            style.spacing.scroll.bar_width = 8.0;
            // Выделяемые надписи перехватывали щелчки: по карточке результата не срабатывали двойной щелчок и меню.
            style.interaction.selectable_labels = false;
            let v = &mut style.visuals;
            v.dark_mode = dark;
            v.panel_fill = p.bg;
            v.window_fill = p.surface;
            v.window_stroke = Stroke::new(LINE_W, p.line);
            v.window_corner_radius = radius(RADIUS_MD);
            v.menu_corner_radius = radius(RADIUS_SM);
            v.extreme_bg_color = p.surface;
            v.faint_bg_color = p.surface_alt;
            v.code_bg_color = p.surface_alt;
            v.hyperlink_color = p.accent;
            v.warn_fg_color = p.warn;
            v.error_fg_color = p.danger;
            v.selection.bg_fill = p.accent.gamma_multiply(if dark { 0.45 } else { 0.25 });
            v.selection.stroke = Stroke::new(LINE_W, p.accent);
            v.window_shadow.color = Color32::from_black_alpha(if dark { 120 } else { 36 });
            v.popup_shadow.color = Color32::from_black_alpha(if dark { 120 } else { 36 });
            let line = Stroke::new(LINE_W, p.line);
            // Единственная линия: ею рисуются и `ui.separator()`, и границы панелей.
            v.widgets.noninteractive.bg_stroke = line;
            v.widgets.noninteractive.bg_fill = p.surface;
            v.widgets.noninteractive.weak_bg_fill = p.surface;
            v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
            v.widgets.inactive.bg_fill = p.surface_alt;
            v.widgets.inactive.weak_bg_fill = p.surface;
            v.widgets.inactive.bg_stroke = line;
            v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
            v.widgets.hovered.bg_fill = p.surface_alt;
            v.widgets.hovered.weak_bg_fill = p.surface_alt;
            v.widgets.hovered.bg_stroke = Stroke::new(LINE_W, p.accent);
            v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
            v.widgets.active.bg_fill = p.selected;
            v.widgets.active.weak_bg_fill = p.selected;
            v.widgets.active.bg_stroke = Stroke::new(LINE_W, p.accent);
            v.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
            v.widgets.open.bg_fill = p.surface_alt;
            v.widgets.open.weak_bg_fill = p.surface_alt;
            v.widgets.open.bg_stroke = line;
            v.widgets.open.fg_stroke = Stroke::new(1.0, p.text);
            for w in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                w.corner_radius = radius(RADIUS_SM);
                w.expansion = 0.0;
            }
        });
    }
}

// ------------------------------------------------------------------ общие элементы

/// Вид сообщения: полоса, всплывающее сообщение.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Info,
    Success,
    Error,
}

impl Kind {
    pub fn color(self, pal: &Palette) -> Color32 {
        match self {
            Kind::Info => pal.accent,
            Kind::Success => pal.ok,
            Kind::Error => pal.danger,
        }
    }

    pub fn soft(self, pal: &Palette) -> Color32 {
        match self {
            Kind::Info => pal.selected,
            Kind::Success => pal.ok_soft,
            Kind::Error => pal.danger_soft,
        }
    }
}

fn button_text(text: &str, color: Color32) -> RichText {
    RichText::new(text).size(FONT_BODY).color(color)
}

/// Главная кнопка экрана: залита цветом акцента.
pub fn primary_button(ui: &mut Ui, pal: &Palette, text: &str) -> Response {
    ui.add(
        egui::Button::new(button_text(text, pal.on_accent))
            .fill(pal.accent)
            .stroke(Stroke::NONE)
            .corner_radius(radius(RADIUS_SM))
            .min_size(vec2(0.0, CONTROL_H)),
    )
}

/// Обычная кнопка.
pub fn button(ui: &mut Ui, pal: &Palette, text: &str) -> Response {
    ui.add(
        egui::Button::new(button_text(text, pal.text)).corner_radius(radius(RADIUS_SM)).min_size(vec2(0.0, CONTROL_H)),
    )
}

/// Кнопка без рамки: второстепенное действие.
pub fn ghost_button(ui: &mut Ui, pal: &Palette, text: &str) -> Response {
    ui.add(
        egui::Button::new(button_text(text, pal.text_2))
            .frame(false)
            .corner_radius(radius(RADIUS_SM))
            .min_size(vec2(0.0, CONTROL_H)),
    )
}

/// Красная кнопка подтверждения опасного действия; текст на ней читается в обеих темах.
pub fn danger_button(ui: &mut Ui, pal: &Palette, text: &str) -> Response {
    ui.add(
        egui::Button::new(button_text(text, pal.on_danger))
            .fill(pal.danger_solid)
            .stroke(Stroke::NONE)
            .corner_radius(radius(RADIUS_SM))
            .min_size(vec2(0.0, CONTROL_H)),
    )
}

/// Кнопка-«таблетка» фильтра или переключатель вида: выбранная окрашена акцентом.
pub fn pill(ui: &mut Ui, pal: &Palette, text: &str, on: bool) -> Response {
    let (fill, fg, line) = if on { (pal.selected, pal.accent, pal.accent) } else { (pal.surface, pal.text, pal.line) };
    ui.add(
        egui::Button::new(button_text(text, fg))
            .fill(fill)
            .stroke(Stroke::new(LINE_W, line))
            .corner_radius(radius(RADIUS_LG))
            .min_size(vec2(0.0, CONTROL_H)),
    )
}

/// Выключатель «вкл/выкл».
pub fn toggle(ui: &mut Ui, pal: &Palette, on: &mut bool) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(44.0, 24.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, ""));
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let off_fill = if pal.dark { pal.gutter } else { rgb(0x7c8598) };
        let fill = off_fill.lerp_to_gamma(pal.accent, t);
        let fill = if ui.is_enabled() { fill } else { fill.gamma_multiply(0.45) };
        let painter = ui.painter();
        painter.rect_filled(rect, radius(12), fill);
        let knob_x = egui::lerp(rect.left() + 12.0..=rect.right() - 12.0, t);
        let knob = if pal.dark && *on { pal.on_accent } else { Color32::WHITE };
        painter.circle_filled(egui::pos2(knob_x, rect.center().y), 9.0, knob);
        if response.has_focus() {
            painter.rect_stroke(rect.expand(2.0), radius(14), Stroke::new(2.0, pal.accent), egui::StrokeKind::Outside);
        }
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Разделитель: линия цвета палитры на всю ширину.
pub fn separator(ui: &mut Ui, pal: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), LINE_W), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(LINE_W, pal.line));
}

/// Полоса хода: тонкая, закруглённая.
pub fn progress(ui: &mut Ui, pal: &Palette, frac: f32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 16.0), Sense::hover());
    let bar = egui::Rect::from_center_size(rect.center(), vec2(width, 8.0));
    ui.painter().rect_filled(bar, radius(4), pal.line);
    let mut done = bar;
    done.set_width((bar.width() * frac.clamp(0.0, 1.0)).max(if frac > 0.0 { 8.0 } else { 0.0 }));
    ui.painter().rect_filled(done, radius(4), pal.accent);
}

/// Небольшая рамка-метка: «сеть», «Лист «Продажи»».
pub fn chip(ui: &mut Ui, text: &str, color: Color32, bg: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font(FONT_CAPTION), color);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(SPACE_SM * 1.5, SPACE_XS), Sense::hover());
    ui.painter().rect_filled(rect, radius(RADIUS_SM), bg);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
}

/// Значок типа файла: цветная плитка с расширением.
pub fn file_tile(ui: &mut Ui, label: &str, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect_filled(rect, radius(RADIUS_SM), color);
    let text: String = label.chars().take(4).collect();
    let galley = ui.painter().layout_no_wrap(text, font(FONT_CAPTION), Color32::WHITE);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, Color32::WHITE);
}

/// Клавиша на «клавиатурной шапке»: подсказка о горячих клавишах.
pub fn keycap(ui: &mut Ui, pal: &Palette, text: &str) {
    let galley = ui.painter().layout_no_wrap(text.to_string(), font(FONT_CAPTION), pal.text);
    let (rect, _) = ui.allocate_exact_size(galley.size() + vec2(SPACE_SM * 1.5, SPACE_XS + 2.0), Sense::hover());
    ui.painter().rect_filled(rect, radius(6), pal.surface_alt);
    ui.painter().rect_stroke(rect, radius(6), Stroke::new(LINE_W, pal.line), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, pal.text);
}

/// Рисованные значки для пустых состояний: в системных шрифтах нужных символов может не оказаться.
#[derive(Clone, Copy)]
pub enum Icon {
    Search,
    Folder,
    Warning,
}

/// Крупный значок в круге.
pub fn icon_badge(ui: &mut Ui, pal: &Palette, icon: Icon) {
    let size = 64.0;
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let (bg, fg) = match icon {
        Icon::Warning => (pal.warn_soft, pal.warn),
        _ => (pal.selected, pal.accent),
    };
    let painter = ui.painter();
    painter.circle_filled(rect.center(), size / 2.0, bg);
    let stroke = Stroke::new(3.0, fg);
    let c = rect.center();
    match icon {
        Icon::Search => {
            let lens = c - vec2(3.0, 3.0);
            painter.circle_stroke(lens, 11.0, stroke);
            painter.line_segment([lens + vec2(8.0, 8.0), lens + vec2(17.0, 17.0)], stroke);
        }
        Icon::Folder => {
            let body = egui::Rect::from_center_size(c + vec2(0.0, 2.0), vec2(30.0, 21.0));
            painter.rect_stroke(body, radius(4), stroke, egui::StrokeKind::Middle);
            painter.line_segment([body.left_top() + vec2(0.0, -1.0), body.left_top() + vec2(10.0, -1.0)], stroke);
        }
        Icon::Warning => {
            let top = c + vec2(0.0, -14.0);
            let (l, r) = (c + vec2(-15.0, 12.0), c + vec2(15.0, 12.0));
            painter.add(egui::Shape::closed_line(vec![top, r, l], stroke));
            painter.line_segment([c + vec2(0.0, -4.0), c + vec2(0.0, 3.0)], stroke);
            painter.circle_filled(c + vec2(0.0, 7.5), 1.8, fg);
        }
    }
}

/// Кнопка-крестик (очистить, закрыть): цель нажатия — целый квадрат `CONTROL_H`, крест нарисован кистью.
pub fn close_button(ui: &mut Ui, pal: &Palette) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(CONTROL_H, CONTROL_H), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect.shrink(2.0), radius(RADIUS_SM), pal.surface_alt);
    }
    let color = if response.hovered() { pal.text } else { pal.muted };
    let c = rect.center();
    let stroke = Stroke::new(1.8, color);
    ui.painter().line_segment([c + vec2(-5.0, -5.0), c + vec2(5.0, 5.0)], stroke);
    ui.painter().line_segment([c + vec2(-5.0, 5.0), c + vec2(5.0, -5.0)], stroke);
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Значок вида сообщения: галочка, восклицательный знак или «i» в круге.
pub fn kind_icon(ui: &mut Ui, pal: &Palette, kind: Kind) {
    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
    let color = kind.color(pal);
    let c = rect.center();
    let painter = ui.painter();
    painter.circle_filled(c, 11.0, color);
    let stroke = Stroke::new(2.0, if pal.dark { pal.on_accent } else { Color32::WHITE });
    match kind {
        Kind::Success => {
            painter.line_segment([c + vec2(-4.5, 0.5), c + vec2(-1.5, 3.5)], stroke);
            painter.line_segment([c + vec2(-1.5, 3.5), c + vec2(4.5, -3.5)], stroke);
        }
        Kind::Error => {
            painter.line_segment([c + vec2(0.0, -5.5), c + vec2(0.0, 1.5)], stroke);
            painter.circle_filled(c + vec2(0.0, 5.0), 1.3, stroke.color);
        }
        Kind::Info => {
            painter.line_segment([c + vec2(0.0, -1.0), c + vec2(0.0, 5.5)], stroke);
            painter.circle_filled(c + vec2(0.0, -5.0), 1.3, stroke.color);
        }
    }
}

/// Строка настройки: слева название и пояснение, справа элемент управления.
pub fn setting_row(ui: &mut Ui, pal: &Palette, title: &str, description: &str, control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
            control(ui);
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = SPACE_XS;
                ui.label(body(pal, title));
                if !description.is_empty() {
                    ui.add(egui::Label::new(small(pal, description)).wrap());
                }
            });
        });
    });
}

/// Подтверждение опасного действия в строке: вопрос, красная кнопка и «Отмена».
/// Возвращает `Some(true)` — подтвердили, `Some(false)` — отказались.
pub fn confirm_row(ui: &mut Ui, pal: &Palette, question: &str, yes: &str) -> Option<bool> {
    let mut answer = None;
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(question).size(FONT_BODY).color(pal.danger));
        if danger_button(ui, pal, yes).clicked() {
            answer = Some(true);
        }
        if button(ui, pal, "Отмена").clicked() {
            answer = Some(false);
        }
    });
    answer
}

/// Карточка: поверхность с рамкой и скруглением.
pub fn card_frame(pal: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(LINE_W, pal.line))
        .corner_radius(radius(RADIUS_MD))
        .inner_margin(egui::Margin::same(SPACE_LG as i8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lum(c: Color32) -> f64 {
        let f = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn text_pairs_have_enough_contrast() {
        for p in [LIGHT, DARK] {
            for bg in [p.bg, p.surface, p.surface_alt, p.hover, p.selected] {
                for fg in [p.text, p.text_2, p.muted, p.accent, p.ok, p.warn, p.danger] {
                    assert!(contrast(fg, bg) >= 4.5, "dark={} {fg:?} on {bg:?}: {:.2}", p.dark, contrast(fg, bg));
                }
            }
            for (fg, bg) in [
                (p.on_accent, p.accent),
                (p.on_danger, p.danger_solid),
                (p.mark_fg, p.mark_bg),
                (p.ok, p.ok_soft),
                (p.warn, p.warn_soft),
                (p.danger, p.danger_soft),
                (p.accent, p.selected),
                (p.text, p.line_hl),
            ] {
                assert!(contrast(fg, bg) >= 4.5, "dark={} {fg:?} on {bg:?}: {:.2}", p.dark, contrast(fg, bg));
            }
        }
    }

    #[test]
    fn file_tiles_keep_white_text_readable() {
        for dark in [false, true] {
            for ext in ["docx", "xlsx", "pptx", "pdf", "txt", "zzz"] {
                let (color, _) = badge(ext, dark);
                assert!(
                    contrast(Color32::WHITE, color) >= 4.5,
                    "{ext} dark={dark}: {:.2}",
                    contrast(Color32::WHITE, color)
                );
            }
        }
    }
}
