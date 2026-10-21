use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use eframe::egui::{
    self, Align, Align2, FontFamily, Id, Key, Layout, Margin, Modifiers, Order, RichText, Stroke,
    ThemePreference, Ui,
};

use crate::{
    audible::Region,
    ffmpeg::{self, Tools},
    session::{Notice, Session},
    session_ui::centered,
    settings::{Appearance, Settings, available_cpus},
    source::{self, SourceBook},
    task::Task,
    theme::{
        self, SEMIBOLD, card, icon, icon_button, muted, palette, primary_button, progress_bar,
        section,
    },
};

const APP_ICON: &[u8] = include_bytes!("../assets/folio.png");

enum ToolsState {
    Ready(Arc<Tools>),
    Downloading {
        progress: Arc<Mutex<f32>>,
        task: Task<Tools>,
    },
    Failed(String),
}

fn start_ffmpeg_download(ctx: &egui::Context) -> ToolsState {
    let progress = Arc::new(Mutex::new(0.0));
    let shared = progress.clone();
    let repaint = ctx.clone();
    let task = Task::spawn(ctx, move || {
        ffmpeg::download(|fraction| {
            *shared.lock().unwrap() = fraction;
            repaint.request_repaint();
        })
    });
    ToolsState::Downloading { progress, task }
}

struct Scan {
    dir: PathBuf,
    progress: Arc<Mutex<(usize, usize)>>,
    task: Task<SourceBook>,
}

struct Toast {
    notice: Notice,
    shown: Instant,
}

impl Toast {
    fn expired(&self) -> bool {
        let lifetime = match self.notice {
            Notice::Success(_) => 5,
            Notice::Error(_) => 12,
        };
        self.shown.elapsed() > Duration::from_secs(lifetime)
    }
}

pub struct App {
    settings: Settings,
    tools: ToolsState,
    /// A folder chosen while ffmpeg was still downloading.
    pending_dir: Option<PathBuf>,
    scan: Option<Scan>,
    session: Option<Session>,
    show_settings: bool,
    toasts: Vec<Toast>,
    styled: bool,
    window_title: String,
}

impl App {
    /// `initial_dir` is a folder passed on the command line, e.g. by dropping it onto folio.exe.
    pub fn new(cc: &eframe::CreationContext<'_>, initial_dir: Option<PathBuf>) -> Self {
        let settings = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, eframe::APP_KEY))
            .unwrap_or_default();
        let tools = match ffmpeg::locate() {
            Some(tools) => ToolsState::Ready(Arc::new(tools)),
            None => start_ffmpeg_download(&cc.egui_ctx),
        };
        let mut app = Self::with_state(settings, tools);
        if let Some(dir) = initial_dir {
            app.open_folder(&cc.egui_ctx, dir);
        }
        app
    }

    fn with_state(settings: Settings, tools: ToolsState) -> Self {
        Self {
            settings,
            tools,
            pending_dir: None,
            scan: None,
            session: None,
            show_settings: false,
            toasts: Vec::new(),
            styled: false,
            window_title: String::new(),
        }
    }

    fn tools(&self) -> Option<Arc<Tools>> {
        match &self.tools {
            ToolsState::Ready(tools) => Some(tools.clone()),
            _ => None,
        }
    }

    fn busy(&self) -> bool {
        self.scan.is_some() || self.session.as_ref().is_some_and(Session::is_building)
    }

    fn apply_appearance(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.settings.appearance {
            Appearance::System => ThemePreference::System,
            Appearance::Light => ThemePreference::Light,
            Appearance::Dark => ThemePreference::Dark,
        });
    }

    fn open_folder(&mut self, ctx: &egui::Context, dir: PathBuf) {
        self.settings.remember(dir.clone());
        let Some(tools) = self.tools() else {
            self.pending_dir = Some(dir);
            return;
        };
        let progress = Arc::new(Mutex::new((0, 0)));
        let shared = progress.clone();
        let repaint = ctx.clone();
        let scan_dir = dir.clone();
        let task = Task::spawn(ctx, move || {
            source::scan(&scan_dir, &tools, &|done, total| {
                *shared.lock().unwrap() = (done, total);
                repaint.request_repaint();
            })
        });
        self.session = None;
        self.scan = Some(Scan {
            dir,
            progress,
            task,
        });
    }

    fn pick_folder(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().set_title("Choose an audiobook folder");
        if let Some(parent) = self.settings.recent.first().and_then(|dir| dir.parent()) {
            dialog = dialog.set_directory(parent);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.open_folder(ctx, dir);
        }
    }

    fn toast(&mut self, notice: Notice) {
        self.toasts.push(Toast {
            notice,
            shown: Instant::now(),
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let ToolsState::Downloading { task, .. } = &self.tools
            && let Some(result) = task.poll()
        {
            match result {
                Ok(tools) => {
                    self.tools = ToolsState::Ready(Arc::new(tools));
                    self.toast(Notice::Success("ffmpeg is installed".to_owned()));
                    if let Some(dir) = self.pending_dir.take() {
                        self.open_folder(ctx, dir);
                    }
                }
                Err(error) => self.tools = ToolsState::Failed(format!("{error:#}")),
            }
        }
        if let Some(scan) = &self.scan
            && let Some(result) = scan.task.poll()
        {
            self.scan = None;
            match result {
                Ok(source) => {
                    let mut session = Session::new(source);
                    session.start_lookup(ctx, &self.settings.regions);
                    self.session = Some(session);
                }
                Err(error) => self.toast(Notice::Error(format!("{error:#}"))),
            }
        }
        let notices = self.session.as_mut().map(Session::poll).unwrap_or_default();
        for notice in notices {
            self.toast(notice);
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .first()
                .map(|file| file.path().to_path_buf())
        });
        if let Some(path) = dropped
            && !self.busy()
        {
            let dir = if path.is_dir() {
                Some(path)
            } else {
                path.parent().map(Path::to_path_buf)
            };
            if let Some(dir) = dir {
                self.open_folder(ctx, dir);
            }
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::O)) && !self.busy() {
            self.pick_folder(ctx);
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.session {
            Some(session) if !session.meta.title.trim().is_empty() => {
                format!("{} — Folio", session.meta.title.trim())
            }
            _ => "Folio".to_owned(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    /// Draws the whole window.
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if !self.styled {
            // New fonts only take effect in the next pass, so this one stays empty.
            theme::install(&ctx);
            egui_extras::install_image_loaders(&ctx);
            self.apply_appearance(&ctx);
            self.styled = true;
            ctx.request_repaint();
            return;
        }
        self.poll(&ctx);
        self.handle_input(&ctx);
        self.update_title(&ctx);

        let p = palette(ui);
        let bar = egui::Frame::new()
            .fill(p.surface)
            .inner_margin(Margin::symmetric(20, 10));
        egui::Panel::top("header")
            .frame(bar)
            .show(ui, |ui| self.header(ui));
        let tools = self.tools();
        let settings = &self.settings;
        let mut footer_height = 0.0;
        match &mut self.session {
            Some(session) => {
                let footer = egui::Frame::new()
                    .fill(p.surface)
                    .inner_margin(Margin::symmetric(20, 0));
                let bar = egui::Panel::bottom("actions")
                    .frame(footer)
                    .show(ui, |ui| session.action_bar(ui, tools, settings));
                footer_height = bar.response.rect.height();
                egui::CentralPanel::no_frame().show(ui, |ui| session.page(ui, settings));
            }
            None => {
                egui::CentralPanel::no_frame().show(ui, |ui| self.welcome(ui));
            }
        }
        self.drop_overlay(&ctx);
        self.settings_modal(&ctx);
        self.show_toasts(&ctx, footer_height);
    }

    fn header(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        ui.horizontal(|ui| {
            ui.add(
                egui::Image::from_bytes("bytes://folio-icon.png", APP_ICON)
                    .fit_to_exact_size(egui::vec2(26.0, 26.0)),
            );
            ui.label(
                RichText::new("Folio")
                    .size(19.0)
                    .family(FontFamily::Name(SEMIBOLD.into())),
            );
            ui.add_space(18.0);
            let busy = self.busy();
            let open = ui
                .add_enabled(
                    !busy,
                    egui::Button::new(format!("{}   Open folder", icon::FOLDER)),
                )
                .on_hover_text("Ctrl+O · or drop a folder onto the window");
            if open.clicked() {
                self.pick_folder(ui.ctx());
            }
            if !self.settings.recent.is_empty() {
                ui.add_enabled_ui(!busy, |ui| {
                    ui.menu_button(format!("{}   Recent", icon::HISTORY), |ui| {
                        for dir in self.settings.recent.clone() {
                            let name = folder_name(&dir);
                            if ui
                                .button(name)
                                .on_hover_text(dir.display().to_string())
                                .clicked()
                            {
                                self.open_folder(ui.ctx(), dir);
                                ui.close();
                            }
                        }
                    });
                });
            }
            if let Some(session) = &self.session {
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(
                        muted(ui, session.source.dir.display().to_string()).size(12.5),
                    )
                    .truncate(),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, icon::SETTINGS, "Settings").clicked() {
                    self.show_settings = true;
                }
                self.tools_status(ui, p);
            });
        });
    }

    fn tools_status(&mut self, ui: &mut Ui, p: theme::Palette) {
        let mut retry = false;
        match &self.tools {
            ToolsState::Ready(tools) => {
                ui.label(
                    muted(
                        ui,
                        format!(
                            "{}  {} encoders",
                            icon::LIGHTNING,
                            self.settings.worker_count()
                        ),
                    )
                    .size(12.5),
                )
                .on_hover_text(format!("Encoding uses {}", tools.ffmpeg.display()));
            }
            ToolsState::Downloading { progress, .. } => {
                let fraction = *progress.lock().unwrap();
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(200.0)
                        .fill(p.accent)
                        .text(format!("Downloading ffmpeg {:.0}%", fraction * 100.0)),
                );
            }
            ToolsState::Failed(error) => {
                retry = ui.button(format!("{}  Retry", icon::REFRESH)).clicked();
                ui.colored_label(p.danger, format!("{}  ffmpeg download failed", icon::ERROR))
                    .on_hover_text(error);
            }
        }
        if retry {
            self.tools = start_ffmpeg_download(ui.ctx());
        }
    }

    fn welcome(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space((ui.available_height() * 0.1).max(24.0));
            centered(ui, 620.0, |ui| {
                if let Some(scan) = &self.scan {
                    let (done, total) = *scan.progress.lock().unwrap();
                    card(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(12.0);
                            ui.add(egui::Spinner::new().size(28.0));
                            ui.add_space(6.0);
                            ui.label(RichText::new("Reading the folder").text_style(egui::TextStyle::Heading));
                            ui.label(muted(ui, scan.dir.display().to_string()));
                            ui.add_space(8.0);
                            let fraction = if total == 0 { 0.0 } else { done as f32 / total as f32 };
                            progress_bar(ui, fraction, ui.available_width(), 8.0);
                            ui.label(muted(ui, format!("{done} of {total} files")).size(12.5));
                            ui.add_space(8.0);
                        });
                    });
                    return;
                }
                let mut pick = false;
                card(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(16.0);
                        let logo = egui::Image::from_bytes("bytes://folio-icon.png", APP_ICON);
                        ui.add(logo.fit_to_exact_size(egui::vec2(88.0, 88.0)));
                        ui.add_space(6.0);
                        let heading = "Turn a folder of MP3s into a finished audiobook";
                        ui.label(RichText::new(heading).size(22.0).family(FontFamily::Name(SEMIBOLD.into())));
                        ui.add_space(2.0);
                        ui.label(muted(
                            ui,
                            "Folio joins the files, looks the book up on Audible, adds chapters and cover art, \
                             and files the M4B into your library.",
                        ));
                        ui.add_space(14.0);
                        if self.pending_dir.is_some() {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(muted(ui, "Waiting for the ffmpeg download to finish…"));
                            });
                        } else {
                            let choose = primary_button(ui, format!("{}   Choose a book folder", icon::FOLDER));
                            pick = ui.add(choose.min_size(egui::vec2(240.0, 42.0))).clicked();
                            ui.label(muted(ui, "or drop a folder anywhere in this window").size(12.5));
                        }
                        ui.add_space(12.0);
                    });
                });
                if pick {
                    self.pick_folder(ui.ctx());
                }
                if !self.settings.recent.is_empty() {
                    ui.add_space(6.0);
                    let mut open = None;
                    section(ui, icon::HISTORY, "Recent folders", |ui| {
                        for dir in &self.settings.recent {
                            let parent = dir.parent().map(|parent| parent.display().to_string()).unwrap_or_default();
                            let response = ui
                                .horizontal(|ui| {
                                    ui.label(muted(ui, icon::FOLDER));
                                    ui.label(folder_name(dir));
                                    ui.add(egui::Label::new(muted(ui, parent).size(12.5)).truncate());
                                })
                                .response
                                .interact(egui::Sense::click())
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if response.clicked() {
                                open = Some(dir.clone());
                            }
                        }
                    });
                    if let Some(dir) = open {
                        self.open_folder(ui.ctx(), dir);
                    }
                }
                ui.add_space(10.0);
                ui.label(muted(ui, format!("{}  Library: {}", icon::LIBRARY, self.settings.library_root)).size(12.5));
            });
        });
    }

    fn drop_overlay(&self, ctx: &egui::Context) {
        let hovering = ctx.input(|input| !input.raw.hovered_files.is_empty());
        if !hovering || self.busy() {
            return;
        }
        egui::Area::new(Id::new("drop_overlay"))
            .order(Order::Foreground)
            .fixed_pos(egui::Pos2::ZERO)
            .show(ctx, |ui| {
                let rect = ctx.content_rect();
                let p = palette(ui);
                let painter = ui.painter();
                painter.rect_filled(rect, 0.0, p.background.gamma_multiply(0.85));
                painter.rect_stroke(
                    rect.shrink(16.0),
                    16.0,
                    Stroke::new(2.0, p.accent),
                    egui::StrokeKind::Inside,
                );
                let text = format!("{}   Drop to open this folder", icon::FOLDER);
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(24.0),
                    p.accent,
                );
            });
    }

    fn settings_modal(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut done = false;
        let mut redownload = false;
        let appearance = self.settings.appearance;
        let frame = egui::Frame::window(&ctx.style_of(ctx.theme())).inner_margin(Margin::same(24));
        let response = egui::Modal::new(Id::new("settings")).frame(frame).show(ctx, |ui| {
            ui.set_width(560.0);
            let p = palette(ui);
            ui.label(RichText::new("Settings").size(22.0).family(FontFamily::Name(SEMIBOLD.into())));
            let settings = &mut self.settings;

            settings_heading(ui, icon::LIBRARY, "Library folder");
            ui.horizontal(|ui| {
                let path = egui::TextEdit::singleline(&mut settings.library_root).margin(Margin::symmetric(8, 6));
                ui.add(path.desired_width(440.0));
                if ui.button("Browse…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    settings.library_root = dir.display().to_string();
                }
            });
            ui.label(muted(ui, "Books go to Author\\Series\\Year - Title\\ inside this folder.").size(12.5));

            settings_heading(ui, icon::GLOBE, "Audible marketplaces");
            ui.horizontal_wrapped(|ui| {
                for region in Region::ALL {
                    let mut enabled = settings.regions.contains(&region);
                    if ui.toggle_value(&mut enabled, region.label()).changed() {
                        if enabled {
                            settings.regions.push(region);
                            settings.regions.sort();
                        } else if settings.regions.len() > 1 {
                            settings.regions.retain(|r| *r != region);
                        }
                    }
                }
            });

            settings_heading(ui, icon::LIGHTNING, "Encoding");
            let cpus = available_cpus();
            let mut auto = settings.workers == 0;
            ui.horizontal(|ui| {
                if ui.checkbox(&mut auto, format!("Use every CPU thread ({cpus})")).changed() {
                    settings.workers = if auto { 0 } else { cpus };
                }
                if !auto {
                    ui.add(egui::Slider::new(&mut settings.workers, 1..=cpus).text("encoders"));
                }
            });
            ui.checkbox(&mut settings.low_priority, "Run encoders at low priority");
            ui.label(
                muted(ui, "Keeps games and other apps smooth while a book encodes. Idle CPU time is still used.")
                    .size(12.5),
            );

            settings_heading(ui, icon::SETTINGS, "Appearance");
            ui.horizontal(|ui| {
                for option in Appearance::ALL {
                    ui.selectable_value(&mut settings.appearance, option, option.label());
                }
            });

            settings_heading(ui, icon::AUDIO, "ffmpeg");
            match &self.tools {
                ToolsState::Ready(tools) => {
                    ui.horizontal(|ui| {
                        ui.colored_label(p.success, icon::CHECK);
                        ui.label(muted(ui, tools.ffmpeg.display().to_string()).size(12.5));
                    });
                }
                ToolsState::Downloading { progress, .. } => {
                    let fraction = *progress.lock().unwrap();
                    ui.add(egui::ProgressBar::new(fraction).fill(p.accent).text("Downloading…"));
                }
                ToolsState::Failed(error) => {
                    ui.colored_label(p.danger, error);
                    redownload = ui.button("Download again").clicked();
                }
            }

            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                done = ui.add(primary_button(ui, "Done").min_size(egui::vec2(96.0, 34.0))).clicked();
            });
        });
        if redownload {
            self.tools = start_ffmpeg_download(ctx);
        }
        if self.settings.appearance != appearance {
            self.apply_appearance(ctx);
        }
        if done || response.should_close() {
            self.show_settings = false;
        }
    }

    fn show_toasts(&mut self, ctx: &egui::Context, footer_height: f32) {
        self.toasts.retain(|toast| !toast.expired());
        if self.toasts.is_empty() {
            return;
        }
        let bottom = footer_height + 16.0;
        let mut dismissed = None;
        egui::Area::new(Id::new("toasts"))
            .order(Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-24.0, -bottom))
            .show(ctx, |ui| {
                let p = palette(ui);
                for (i, toast) in self.toasts.iter().enumerate().rev() {
                    let (glyph, color, text) = match &toast.notice {
                        Notice::Success(text) => (icon::CHECK, p.success, text),
                        Notice::Error(text) => (icon::ERROR, p.danger, text),
                    };
                    let response = egui::Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0, p.border))
                        .corner_radius(10)
                        .shadow(ui.visuals().popup_shadow)
                        .inner_margin(Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_max_width(380.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(glyph).color(color).size(16.0));
                                ui.add(egui::Label::new(text.as_str()).wrap());
                            });
                        })
                        .response
                        .interact(egui::Sense::click())
                        .on_hover_text("Click to dismiss");
                    if response.clicked() {
                        dismissed = Some(i);
                    }
                    ui.add_space(8.0);
                }
            });
        if let Some(i) = dismissed {
            self.toasts.remove(i);
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

fn folder_name(dir: &Path) -> String {
    dir.file_name().map_or_else(
        || dir.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn settings_heading(ui: &mut Ui, glyph: &str, text: &str) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(glyph).color(palette(ui).accent));
        ui.label(RichText::new(text).family(FontFamily::Name(SEMIBOLD.into())));
    });
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }
}

/// Renders the main screens to `target/ui-preview/*.png` without opening a window:
/// `cargo test preview -- --ignored`.
#[cfg(test)]
mod preview {
    use std::{collections::HashMap, fs, io::Cursor};

    use egui_kittest::Harness;

    use super::*;
    use crate::{
        audible::{AudibleChapter, AudibleChapters, Product, SeriesRef},
        convert::Stage,
        ffmpeg::Probe,
        nfo,
        picture::Picture,
        session::{AudibleMatch, Tab},
        source::SourceFile,
    };

    /// A plain gradient, so the previews don't need a real cover on disk.
    fn cover() -> Option<Picture> {
        let image = image::RgbImage::from_fn(500, 500, |x, y| {
            image::Rgb([60 + (x / 4) as u8, 70, 110 + (y / 5) as u8])
        });
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .ok()?;
        Some(Picture::new(bytes))
    }

    fn source() -> SourceBook {
        let durations = [
            1328.5, 904.6, 1297.4, 1302.3, 1354.1, 1071.6, 785.7, 1251.6, 123.0, 1082.7, 1212.2,
            1245.7,
        ];
        let files = durations
            .iter()
            .enumerate()
            .map(|(i, &duration)| SourceFile {
                path: PathBuf::from(format!(
                    r"C:\Books\The Faith of Beasts\The Faith of Beasts - {:02}.mp3",
                    i + 1
                )),
                probe: Probe {
                    duration,
                    codec: "mp3".into(),
                    bit_rate: 128_000,
                    channels: 1,
                    sample_rate: 44100,
                    has_picture: true,
                    tags: HashMap::new(),
                },
            })
            .collect();
        let nfo = nfo::parse(
            "General Information\n===================\n Title: The Faith of Beasts\n Author: James S. A. Corey\n Read By: Jefferson Mays\n Chapters: 12\n",
        );
        SourceBook {
            dir: PathBuf::from(r"C:\Books\The Faith of Beasts"),
            files,
            nfo: Some(nfo),
            cover: cover(),
            cover_origin: "The Faith of Beasts.jpg".into(),
            asin: Some(("B0FBCFCBG1".into(), Some(Region::Us))),
        }
    }

    fn product(asin: &str, title: &str, region: Region, minutes: u32) -> Product {
        Product {
            asin: asin.into(),
            region,
            title: title.into(),
            subtitle: String::new(),
            authors: vec!["James S. A. Corey".into()],
            narrators: vec!["Jefferson Mays".into()],
            series: vec![
                SeriesRef { name: "Captive's War".into(), position: "2".into() },
                SeriesRef { name: "The Captive's War".into(), position: "2".into() },
            ],
            publisher: "Recorded Books".into(),
            release_date: "2026-04-14".into(),
            language: "english".into(),
            format_type: "unabridged".into(),
            explicit: false,
            runtime_min: Some(minutes),
            genres: vec![
                vec!["Science Fiction & Fantasy".into(), "Science Fiction".into(), "Space Opera".into()],
                vec!["Science Fiction & Fantasy".into(), "Science Fiction".into(), "First Contact".into()],
            ],
            description: "The monstrous Carryx empire was built by subjugation and war. Thousands of species are \
                bound to their Sovran’s command in an endless, blood-soaked test: be useful in the eternal conflict \
                or be slaughtered.\n\nDafyd Alkhor, highest among their human captives, is feared and despised by \
                the very people he champions."
                .into(),
            copyright: "©2026 Daniel Abraham and Ty Franck (P)2026 Recorded Books".into(),
            cover_url: None,
            thumb_url: None,
        }
    }

    fn session() -> Session {
        let source = source();
        let minutes = (source.total_secs() / 60.0).round() as u32;
        let mut session = Session::new(source);
        let titles = [
            "Opening Credits",
            "Part One: Structure",
            "One",
            "Two",
            "Three",
            "Four",
            "Five",
            "Six",
            "Seven",
            "Eight",
            "Part Two: Antithesis",
            "Nine",
            "Ten",
            "Eleven",
        ];
        let total = session.total_ms();
        let chapters = AudibleChapters {
            runtime_ms: total,
            chapters: titles
                .iter()
                .enumerate()
                .map(|(i, title)| AudibleChapter {
                    start_ms: total * i as u64 / titles.len() as u64,
                    title: (*title).into(),
                })
                .collect(),
        };
        session.hits = vec![
            product("B0FBCFCBG1", "The Faith of Beasts", Region::Us, minutes),
            product("B0FBCFCBG1", "The Faith of Beasts", Region::De, minutes),
            product("B0CR1D12S4", "The Mercy of Gods", Region::Us, 1210),
        ];
        session.searched = true;
        let product = product("B0FBCFCBG1", "The Faith of Beasts", Region::Us, minutes);
        session.apply_match(AudibleMatch {
            product,
            chapters: Some(chapters),
            cover: cover(),
        });
        session
    }

    fn app(theme: egui::Theme) -> App {
        let tools = Tools {
            ffmpeg: PathBuf::from(r"C:\ffmpeg\ffmpeg.exe"),
            ffprobe: PathBuf::from(r"C:\ffmpeg\ffprobe.exe"),
        };
        let settings = Settings {
            recent: vec![
                PathBuf::from(r"C:\Books\The Faith of Beasts"),
                PathBuf::from(r"C:\Books\Project Hail Mary"),
                PathBuf::from(r"\\nas\Books\Der Turm des Wissens"),
            ],
            library_root: r"\\nas\Audiobooks".into(),
            appearance: if theme == egui::Theme::Dark {
                Appearance::Dark
            } else {
                Appearance::Light
            },
            ..Settings::default()
        };
        App::with_state(settings, ToolsState::Ready(Arc::new(tools)))
    }

    fn render(name: &str, theme: egui::Theme, prepare: impl FnOnce(&mut App, &egui::Context)) {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1320.0, 1180.0))
            .with_theme(theme)
            .wgpu()
            .build_ui_state(|ui, app: &mut App| app.show(ui), app(theme));
        harness.step();
        let ctx = harness.ctx.clone();
        prepare(harness.state_mut(), &ctx);
        for _ in 0..8 {
            harness.step();
            std::thread::sleep(Duration::from_millis(50));
        }
        let image = harness.render().expect("render");
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("ui-preview");
        fs::create_dir_all(&dir).unwrap();
        image.save(dir.join(format!("{name}.png"))).unwrap();
    }

    fn with_tab(tab: Tab) -> impl FnOnce(&mut App, &egui::Context) {
        move |app, _| {
            let mut session = session();
            session.tab = tab;
            app.session = Some(session);
        }
    }

    #[test]
    #[ignore]
    fn preview() {
        render("welcome", egui::Theme::Light, |_, _| {});
        render("details", egui::Theme::Light, with_tab(Tab::Details));
        render("details-dark", egui::Theme::Dark, with_tab(Tab::Details));
        render("chapters", egui::Theme::Light, with_tab(Tab::Chapters));
        render("audible", egui::Theme::Light, with_tab(Tab::Audible));
        render("source", egui::Theme::Dark, with_tab(Tab::Source));
        render("building", egui::Theme::Light, |app, ctx| {
            let mut session = session();
            session.preview_build(ctx, Stage::Encoding, 0.63);
            app.session = Some(session);
            app.toast(Notice::Success(
                "Filled in from Audible US: The Faith of Beasts".into(),
            ));
        });
        render("settings", egui::Theme::Dark, |app, _| {
            app.session = Some(session());
            app.show_settings = true;
        });
    }
}
