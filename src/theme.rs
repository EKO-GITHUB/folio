//! Look and feel: Windows fonts and icons, a light and a dark palette, and small styled widgets.

use std::{fs, path::PathBuf, sync::Arc};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Response,
    RichText, Shadow, Stroke, TextStyle, Theme, Ui, Vec2, Visuals, style::WidgetVisuals,
};

/// Glyphs of the Segoe Fluent Icons / Segoe MDL2 Assets fonts that ship with Windows.
pub mod icon {
    pub const ADD: &str = "\u{E710}";
    pub const AUDIO: &str = "\u{E8D6}";
    pub const BOOK: &str = "\u{E736}";
    pub const CALENDAR: &str = "\u{E787}";
    pub const CHECK: &str = "\u{E73E}";
    pub const CLOCK: &str = "\u{E823}";
    pub const CLOSE: &str = "\u{E711}";
    pub const COPY: &str = "\u{E8C8}";
    pub const DELETE: &str = "\u{E74D}";
    pub const DOCUMENT: &str = "\u{E8A5}";
    pub const ERROR: &str = "\u{E783}";
    pub const FOLDER: &str = "\u{ED25}";
    pub const GLOBE: &str = "\u{E774}";
    pub const HISTORY: &str = "\u{E81C}";
    pub const INFO: &str = "\u{E946}";
    pub const LIBRARY: &str = "\u{E8F1}";
    pub const LIGHTNING: &str = "\u{E945}";
    pub const LIST: &str = "\u{E8FD}";
    pub const MIC: &str = "\u{E720}";
    pub const OPEN_EXTERNAL: &str = "\u{E8A7}";
    pub const PERSON: &str = "\u{E77B}";
    pub const PICTURE: &str = "\u{E8B9}";
    pub const PLAY: &str = "\u{E768}";
    pub const REFRESH: &str = "\u{E72C}";
    pub const SEARCH: &str = "\u{E721}";
    pub const SETTINGS: &str = "\u{E713}";
    pub const TAG: &str = "\u{E8EC}";
    pub const WARNING: &str = "\u{E7BA}";

    #[cfg(test)]
    pub const ALL: [&str; 28] = [
        ADD,
        AUDIO,
        BOOK,
        CALENDAR,
        CHECK,
        CLOCK,
        CLOSE,
        COPY,
        DELETE,
        DOCUMENT,
        ERROR,
        FOLDER,
        GLOBE,
        HISTORY,
        INFO,
        LIBRARY,
        LIGHTNING,
        LIST,
        MIC,
        OPEN_EXTERNAL,
        PERSON,
        PICTURE,
        PLAY,
        REFRESH,
        SEARCH,
        SETTINGS,
        TAG,
        WARNING,
    ];
}

pub const SEMIBOLD: &str = "semibold";

/// Colours that egui's `Visuals` has no slot for.
#[derive(Clone, Copy)]
pub struct Palette {
    pub background: Color32,
    pub surface: Color32,
    pub raised: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub highlight: Color32,
}

const LIGHT: Palette = Palette {
    background: Color32::from_rgb(0xF3, 0xF4, 0xF6),
    surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    raised: Color32::from_rgb(0xEE, 0xF0, 0xF3),
    border: Color32::from_rgb(0xE0, 0xE3, 0xE8),
    text: Color32::from_rgb(0x1D, 0x21, 0x27),
    muted: Color32::from_rgb(0x69, 0x71, 0x7D),
    accent: Color32::from_rgb(0x2D, 0x64, 0xD8),
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(0x1A, 0x8A, 0x55),
    warning: Color32::from_rgb(0xB2, 0x6B, 0x0A),
    danger: Color32::from_rgb(0xCC, 0x3B, 0x3B),
    highlight: Color32::from_rgb(0xD9, 0x8E, 0x1C),
};

const DARK: Palette = Palette {
    background: Color32::from_rgb(0x14, 0x16, 0x1A),
    surface: Color32::from_rgb(0x1D, 0x20, 0x25),
    raised: Color32::from_rgb(0x29, 0x2D, 0x34),
    border: Color32::from_rgb(0x30, 0x35, 0x3D),
    text: Color32::from_rgb(0xE7, 0xE9, 0xEC),
    muted: Color32::from_rgb(0x9A, 0xA2, 0xAD),
    accent: Color32::from_rgb(0x5B, 0x8D, 0xEF),
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(0x45, 0xB8, 0x7F),
    warning: Color32::from_rgb(0xE3, 0xA4, 0x3B),
    danger: Color32::from_rgb(0xF0, 0x6B, 0x6B),
    highlight: Color32::from_rgb(0xE8, 0xAB, 0x4A),
};

pub fn palette(ui: &Ui) -> Palette {
    if ui.visuals().dark_mode { DARK } else { LIGHT }
}

/// Loads the Windows UI fonts (falling back to egui's own) and applies both themes.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    for (theme, palette) in [(Theme::Light, LIGHT), (Theme::Dark, DARK)] {
        ctx.style_mut_of(theme, |style| {
            style.visuals = visuals(theme, palette);
            style.text_styles = [
                (TextStyle::Small, FontId::proportional(12.5)),
                (TextStyle::Body, FontId::proportional(14.5)),
                (TextStyle::Button, FontId::proportional(14.0)),
                (TextStyle::Monospace, FontId::monospace(13.5)),
                (
                    TextStyle::Heading,
                    FontId::new(17.0, FontFamily::Name(SEMIBOLD.into())),
                ),
                (
                    TextStyle::Name("title".into()),
                    FontId::new(28.0, FontFamily::Name(SEMIBOLD.into())),
                ),
            ]
            .into();
            let spacing = &mut style.spacing;
            spacing.item_spacing = egui::vec2(8.0, 8.0);
            spacing.button_padding = egui::vec2(12.0, 5.0);
            spacing.interact_size.y = 30.0;
            spacing.window_margin = Margin::same(20);
            spacing.menu_margin = Margin::same(6);
            spacing.combo_width = 120.0;
            spacing.text_edit_width = 280.0;
            spacing.scroll.floating = true;
        });
    }
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let dir = std::env::var_os("WINDIR")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("Fonts");
    let mut load = |key: &str, files: &[&str]| -> bool {
        let data = files.iter().find_map(|file| fs::read(dir.join(file)).ok());
        if let Some(data) = data {
            fonts
                .font_data
                .insert(key.to_owned(), Arc::new(FontData::from_owned(data)));
        }
        fonts.font_data.contains_key(key)
    };
    let regular = load("segoe", &["segoeui.ttf"]);
    let semibold = load("segoe-semibold", &["seguisb.ttf", "segoeuib.ttf"]);
    let icons = load("icons", &["SegoeIcons.ttf", "segmdl2.ttf"]);
    let symbols = load("symbols", &["seguisym.ttf"]);
    let mono = load("mono", &["CascadiaMono.ttf", "consola.ttf"]);

    let defaults = fonts.families[&FontFamily::Proportional].clone();
    let mut fallbacks: Vec<String> = Vec::new();
    fallbacks.extend(icons.then(|| "icons".to_owned()));
    fallbacks.extend(symbols.then(|| "symbols".to_owned()));
    fallbacks.extend(defaults);

    let family = |first: Option<&str>| -> Vec<String> {
        first
            .map(str::to_owned)
            .into_iter()
            .chain(fallbacks.clone())
            .collect()
    };
    let proportional = family(regular.then_some("segoe"));
    let bold = family(
        semibold
            .then_some("segoe-semibold")
            .or(regular.then_some("segoe")),
    );
    fonts
        .families
        .insert(FontFamily::Proportional, proportional);
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), bold);
    if mono {
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, "mono".to_owned());
    }
    fonts
}

fn visuals(theme: Theme, p: Palette) -> Visuals {
    let mut visuals = match theme {
        Theme::Light => Visuals::light(),
        Theme::Dark => Visuals::dark(),
    };
    let radius = CornerRadius::same(5);
    let widget = |fill: Color32, stroke: Color32, text: Color32| WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: Stroke::new(1.0, stroke),
        corner_radius: radius,
        fg_stroke: Stroke::new(1.0, text),
        expansion: 0.0,
    };
    let hover = if theme == Theme::Dark {
        p.raised.gamma_multiply(1.25)
    } else {
        Color32::from_rgb(0xE4, 0xE7, 0xEB)
    };
    let pressed = if theme == Theme::Dark {
        p.raised.gamma_multiply(1.5)
    } else {
        Color32::from_rgb(0xD8, 0xDC, 0xE2)
    };
    let input = if theme == Theme::Dark {
        Color32::from_rgb(0x16, 0x18, 0x1C)
    } else {
        Color32::WHITE
    };

    visuals.widgets.noninteractive = widget(p.surface, p.border, p.text);
    visuals.widgets.inactive = widget(p.raised, p.border, p.text);
    visuals.widgets.hovered = widget(hover, p.border.gamma_multiply(1.4), p.text);
    visuals.widgets.active = widget(pressed, p.accent, p.text);
    visuals.widgets.open = widget(hover, p.border, p.text);
    visuals.selection.bg_fill =
        p.accent
            .gamma_multiply(if theme == Theme::Dark { 0.45 } else { 0.25 });
    visuals.selection.stroke = Stroke::new(1.5, p.accent);
    visuals.hyperlink_color = p.accent;
    visuals.panel_fill = p.background;
    visuals.window_fill = p.surface;
    visuals.window_stroke = Stroke::new(1.0, p.border);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.window_shadow = Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(60),
    };
    visuals.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(40),
    };
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.extreme_bg_color = input;
    visuals.text_edit_bg_color = Some(input);
    visuals.faint_bg_color = if theme == Theme::Dark {
        Color32::from_rgb(0x22, 0x25, 0x2B)
    } else {
        Color32::from_rgb(0xF7, 0xF8, 0xFA)
    };
    visuals.code_bg_color = p.raised;
    visuals.warn_fg_color = p.warning;
    visuals.error_fg_color = p.danger;
    visuals.override_text_color = Some(p.text);
    visuals.weak_text_color = Some(p.muted);
    visuals.striped = false;
    visuals.indent_has_left_vline = false;
    visuals
}

/// A white (or dark) rounded panel that groups related content.
pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(10)
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

/// A card with a heading and an optional icon.
pub fn section<R>(
    ui: &mut Ui,
    glyph: &str,
    title: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> R {
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(glyph).size(16.0).color(palette(ui).accent));
            ui.label(RichText::new(title).text_style(TextStyle::Heading));
        });
        ui.add_space(4.0);
        add_contents(ui)
    })
}

/// A small rounded tag, e.g. for the series or the duration.
/// Painted as a single widget so wrapping layouts move it to the next line as a whole.
pub fn chip(ui: &mut Ui, glyph: &str, text: impl Into<String>, color: Color32) -> Response {
    let label = if glyph.is_empty() {
        text.into()
    } else {
        format!("{glyph}  {}", text.into())
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label, FontId::proportional(13.0), color);
    let padding = egui::vec2(10.0, 4.0);
    let (rect, response) =
        ui.allocate_exact_size(galley.size() + 2.0 * padding, egui::Sense::hover());
    let fill = color.gamma_multiply(if ui.visuals().dark_mode { 0.22 } else { 0.12 });
    ui.painter().rect_filled(rect, rect.height() / 2.0, fill);
    ui.painter().galley(rect.min + padding, galley, color);
    response
}

/// A thin rounded progress bar that stays visible on white cards, unlike egui's.
pub fn progress_bar(ui: &mut Ui, fraction: f32, width: f32, height: f32) -> Response {
    let p = palette(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let radius = height / 2.0;
    ui.painter().rect_filled(rect, radius, p.raised);
    let filled = egui::Rect::from_min_size(
        rect.min,
        egui::vec2(rect.width() * fraction.clamp(0.0, 1.0), height),
    );
    if filled.width() > 0.5 {
        ui.painter().rect_filled(filled, radius, p.accent);
    }
    response
}

pub fn primary_button(ui: &Ui, text: impl Into<String>) -> egui::Button<'static> {
    let p = palette(ui);
    egui::Button::new(
        RichText::new(text.into())
            .color(p.on_accent)
            .size(15.0)
            .family(FontFamily::Name(SEMIBOLD.into())),
    )
    .fill(p.accent)
    .stroke(Stroke::NONE)
    .corner_radius(8)
    .min_size(Vec2::new(0.0, 36.0))
}

/// A borderless button showing just an icon.
pub fn icon_button(ui: &mut Ui, glyph: &str, tooltip: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(glyph).size(15.0))
            .frame_when_inactive(false)
            .min_size(Vec2::splat(30.0)),
    )
    .on_hover_text(tooltip)
}

/// A label above a form field.
pub fn field_label(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.5).color(palette(ui).muted));
}

pub fn muted(ui: &Ui, text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(palette(ui).muted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_font_has_every_glyph() {
        let dir = std::env::var_os("WINDIR")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
            .join("Fonts");
        for file in ["SegoeIcons.ttf", "segmdl2.ttf"] {
            let Ok(data) = fs::read(dir.join(file)) else {
                continue;
            };
            let face = ttf_parser::Face::parse(&data, 0).unwrap();
            for glyph in icon::ALL {
                let c = glyph.chars().next().unwrap();
                assert!(
                    face.glyph_index(c).is_some(),
                    "{file} has no glyph for {:X}",
                    c as u32
                );
            }
        }
    }
}
