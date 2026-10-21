use std::{path::Path, sync::Arc, time::Duration};

use eframe::egui::{
    self, Align, FontFamily, Key, Layout, Margin, Modifiers, Response, RichText, Sense, Stroke,
    TextStyle, Ui, Vec2,
};

use crate::{
    audible::Product,
    chapters::{self, ChapterOrigin},
    convert::{self, Stage},
    ffmpeg::Tools,
    naming::OutputNames,
    picture::Picture,
    session::{Build, ChapterRow, Outcome, Session, Tab},
    settings::Settings,
    text::format_duration,
    theme::{
        SEMIBOLD, card, chip, field_label, icon, icon_button, muted, palette, primary_button,
        progress_bar, section,
    },
};

pub const CONTENT_WIDTH: f32 = 1180.0;
const COVER_SIZE: f32 = 188.0;
/// Audible rounds runtimes to minutes, so a search hit counts as matching within this margin.
const RUNTIME_MATCH_SECS: f64 = 90.0;
const CHAPTER_ROW_HEIGHT: f32 = 32.0;
/// How much of the build each stage takes, roughly.
const STAGE_WEIGHTS: [f32; 4] = [0.08, 0.84, 0.02, 0.06];

/// Lays `add_contents` out in a column of at most `max_width`, centred horizontally.
pub fn centered(ui: &mut Ui, max_width: f32, add_contents: impl FnOnce(&mut Ui)) {
    let width = ui.available_width().min(max_width);
    let margin = ((ui.available_width() - width) / 2.0).max(0.0);
    ui.horizontal_top(|ui| {
        ui.add_space(margin);
        ui.vertical(|ui| {
            ui.set_width(width);
            add_contents(ui);
        });
    });
}

impl Session {
    /// The scrollable page: the book, the tabs and the current tab.
    pub fn page(&mut self, ui: &mut Ui, settings: &Settings) {
        egui::ScrollArea::vertical()
            .id_salt("page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(20.0);
                centered(ui, CONTENT_WIDTH, |ui| {
                    self.hero(ui);
                    ui.add_space(12.0);
                    self.tab_bar(ui);
                    ui.add_space(12.0);
                    match self.tab {
                        Tab::Details => self.details(ui),
                        Tab::Chapters => self.chapter_editor(ui),
                        Tab::Audible => self.audible_tab(ui, settings),
                        Tab::Source => self.source_tab(ui),
                    }
                    ui.add_space(24.0);
                });
            });
    }

    fn hero(&mut self, ui: &mut Ui) {
        card(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(COVER_SIZE);
                    cover_view(ui, self.cover());
                    ui.add_space(6.0);
                    self.cover_switch(ui);
                });
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    let p = palette(ui);
                    let title = if self.meta.title.trim().is_empty() {
                        "Untitled book"
                    } else {
                        &self.meta.title
                    };
                    ui.add(
                        egui::Label::new(
                            RichText::new(title).text_style(TextStyle::Name("title".into())),
                        )
                        .wrap(),
                    );
                    if !self.meta.subtitle.is_empty() {
                        ui.label(RichText::new(&self.meta.subtitle).size(16.0).color(p.muted));
                    }
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        let authors = self.meta.author_list();
                        ui.label(muted(ui, icon::PERSON));
                        ui.label(if authors.is_empty() {
                            "Unknown author".to_owned()
                        } else {
                            authors.join(", ")
                        });
                        let narrators = self.meta.narrator_list();
                        if !narrators.is_empty() {
                            ui.add_space(12.0);
                            ui.label(muted(ui, icon::MIC));
                            ui.label(format!("Narrated by {}", narrators.join(", ")));
                        }
                    });
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                        if !self.meta.series.is_empty() {
                            let part = self.meta.series_part.trim();
                            let text = if part.is_empty() {
                                self.meta.series.clone()
                            } else {
                                format!("{} · Book {part}", self.meta.series)
                            };
                            chip(ui, icon::LIBRARY, text, p.highlight);
                        }
                        chip(
                            ui,
                            icon::CLOCK,
                            format_duration(self.source.total_secs()),
                            p.muted,
                        );
                        if let Some(year) = self.meta.year() {
                            chip(ui, icon::CALENDAR, year, p.muted);
                        }
                        if !self.meta.language.is_empty() {
                            chip(ui, icon::GLOBE, capitalize(&self.meta.language), p.muted);
                        }
                        if !self.meta.publisher.is_empty() {
                            chip(ui, "", &self.meta.publisher, p.muted);
                        }
                        chip(
                            ui,
                            icon::AUDIO,
                            format!("{} files", self.source.files.len()),
                            p.muted,
                        );
                    });
                    ui.add_space(12.0);
                    self.match_status(ui);
                });
            });
        });
    }

    fn cover_switch(&mut self, ui: &mut Ui) {
        let audible = self.audible_cover().map(Picture::describe_size);
        let local = self.source.cover.as_ref().map(Picture::describe_size);
        match (audible, local) {
            (Some(audible), Some(local)) => {
                let options = [
                    (false, format!("Audible {audible}")),
                    (true, format!("Folder {local}")),
                ];
                segmented(ui, &mut self.use_local_cover, &options);
            }
            (Some(size), None) => {
                ui.label(muted(ui, format!("Audible cover · {size}")).size(12.5));
            }
            (None, Some(size)) => {
                ui.label(muted(ui, format!("{} · {size}", self.source.cover_origin)).size(12.5));
            }
            (None, None) => {}
        }
    }

    fn match_status(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        ui.horizontal(|ui| {
            if self.lookup.is_some() {
                ui.spinner();
                ui.label(muted(ui, "Looking the book up on Audible…"));
            } else if let Some(found) = &self.matched {
                let product = &found.product;
                chip(
                    ui,
                    icon::CHECK,
                    format!(
                        "Matched on Audible {} · {}",
                        product.region.label(),
                        product.asin
                    ),
                    p.success,
                );
                if ui.link("Change").clicked() {
                    self.tab = Tab::Audible;
                }
            } else {
                chip(ui, icon::WARNING, "Not matched on Audible yet", p.warning);
                if ui.link("Find it").clicked() {
                    self.tab = Tab::Audible;
                }
            }
        });
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        let tabs = [
            (Tab::Details, icon::DOCUMENT, "Details".to_owned()),
            (
                Tab::Chapters,
                icon::LIST,
                format!("Chapters  {}", self.chapters.len()),
            ),
            (
                Tab::Audible,
                icon::SEARCH,
                if self.hits.is_empty() {
                    "Audible".to_owned()
                } else {
                    format!("Audible  {}", self.hits.len())
                },
            ),
            (
                Tab::Source,
                icon::FOLDER,
                format!("Source  {}", self.source.files.len()),
            ),
        ];
        let response = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 28.0;
            ui.add_space(4.0);
            for (tab, glyph, label) in tabs {
                let selected = self.tab == tab;
                let color = if selected { p.accent } else { p.muted };
                let text = RichText::new(format!("{glyph}  {label}"))
                    .size(14.5)
                    .color(color);
                let text = if selected {
                    text.family(FontFamily::Name(SEMIBOLD.into()))
                } else {
                    text
                };
                let button = ui.add(
                    egui::Button::new(text)
                        .frame(false)
                        .min_size(egui::vec2(0.0, 36.0)),
                );
                if selected {
                    let rect = button.rect;
                    let y = rect.bottom() + 3.0;
                    ui.painter().line_segment(
                        [
                            egui::pos2(rect.left() - 4.0, y),
                            egui::pos2(rect.right() + 4.0, y),
                        ],
                        Stroke::new(2.5, p.accent),
                    );
                }
                if button.clicked() {
                    self.tab = tab;
                }
            }
        });
        let bottom = response.response.rect.bottom() + 4.5;
        ui.painter()
            .hline(ui.max_rect().x_range(), bottom, Stroke::new(1.0, p.border));
    }

    fn details(&mut self, ui: &mut Ui) {
        if ui.available_width() > 860.0 {
            ui.spacing_mut().item_spacing.x = 14.0;
            ui.columns_const(|[left, right]| {
                self.book_card(left);
                left.add_space(6.0);
                self.series_card(left);
                self.publication_card(right);
                right.add_space(6.0);
                self.genres_card(right);
            });
            ui.spacing_mut().item_spacing.x = 8.0;
        } else {
            self.book_card(ui);
            self.series_card(ui);
            self.publication_card(ui);
            self.genres_card(ui);
        }
        ui.add_space(6.0);
        self.description_card(ui);
    }

    fn book_card(&mut self, ui: &mut Ui) {
        let meta = &mut self.meta;
        section(ui, icon::BOOK, "Book", |ui| {
            text_field(ui, "Title", &mut meta.title, "");
            text_field(ui, "Subtitle", &mut meta.subtitle, "Optional");
            text_field(ui, "Authors", &mut meta.authors, "Comma separated");
            ui.label(muted(ui, "The first author names the folder in your library.").size(12.0));
            ui.add_space(2.0);
            text_field(ui, "Narrators", &mut meta.narrators, "Comma separated");
        });
    }

    fn series_card(&mut self, ui: &mut Ui) {
        let alternatives: Vec<(String, String)> = self
            .matched
            .as_ref()
            .map(|found| {
                found
                    .product
                    .series
                    .iter()
                    .map(|s| (s.name.clone(), s.position.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let meta = &mut self.meta;
        section(ui, icon::LIBRARY, "Series", |ui| {
            ui.horizontal_top(|ui| {
                let width = ui.available_width();
                ui.vertical(|ui| {
                    ui.set_width(width - 110.0);
                    text_field(ui, "Name", &mut meta.series, "Not part of a series");
                });
                ui.vertical(|ui| {
                    ui.set_width(94.0);
                    text_field(ui, "Book", &mut meta.series_part, "#");
                });
            });
            let others: Vec<&(String, String)> = alternatives
                .iter()
                .filter(|(name, part)| *name != meta.series || *part != meta.series_part)
                .collect();
            if !others.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(muted(ui, "Audible also lists it in").size(12.5));
                    for (name, part) in others {
                        let text = if part.is_empty() {
                            name.clone()
                        } else {
                            format!("{name} #{part}")
                        };
                        if ui
                            .small_button(text)
                            .on_hover_text("Use this series")
                            .clicked()
                        {
                            meta.series = name.clone();
                            meta.series_part = part.clone();
                        }
                    }
                });
            }
        });
    }

    fn publication_card(&mut self, ui: &mut Ui) {
        let meta = &mut self.meta;
        section(ui, icon::CALENDAR, "Publication", |ui| {
            two_fields(
                ui,
                ("Release date", &mut meta.release_date, "YYYY-MM-DD"),
                ("Language", &mut meta.language, "english"),
            );
            two_fields(
                ui,
                ("Publisher", &mut meta.publisher, ""),
                ("ASIN", &mut meta.asin, "B0…"),
            );
            text_field(ui, "Copyright", &mut meta.copyright, "");
            ui.checkbox(&mut meta.explicit, "Explicit content");
        });
    }

    fn genres_card(&mut self, ui: &mut Ui) {
        let mut genres = self.genres();
        let mut changed = false;
        section(ui, icon::TAG, "Genres", |ui| {
            let p = palette(ui);
            if genres.is_empty() {
                ui.label(muted(ui, "No genres yet"));
            }
            let mut remove = None;
            for (i, genre) in genres.iter().enumerate() {
                let fill =
                    p.accent
                        .gamma_multiply(if ui.visuals().dark_mode { 0.16 } else { 0.07 });
                let width = ui.available_width() - 20.0;
                egui::Frame::new()
                    .fill(fill)
                    .corner_radius(6)
                    .inner_margin(Margin::symmetric(10, 2))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.horizontal(|ui| {
                            let mut levels: Vec<&str> = genre.split('/').map(str::trim).collect();
                            let last = levels.pop().unwrap_or_default();
                            let path = levels.join(" › ");
                            ui.label(RichText::new(last).color(p.accent));
                            if !path.is_empty() {
                                ui.add(egui::Label::new(muted(ui, path).size(12.5)).truncate());
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if icon_button(ui, icon::CLOSE, "Remove this genre").clicked() {
                                    remove = Some(i);
                                }
                            });
                        });
                    });
            }
            if let Some(i) = remove {
                genres.remove(i);
                changed = true;
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.new_genre)
                        .hint_text("Add a genre, e.g. Fantasy / Epic Fantasy")
                        .desired_width(ui.available_width() - 96.0)
                        .margin(Margin::symmetric(8, 6)),
                );
                let entered = input.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (ui.button(format!("{}  Add", icon::ADD)).clicked() || entered)
                    && !self.new_genre.trim().is_empty()
                {
                    genres.push(self.new_genre.trim().to_owned());
                    self.new_genre.clear();
                    changed = true;
                }
            });
        });
        if changed {
            self.set_genres(&genres);
        }
    }

    fn description_card(&mut self, ui: &mut Ui) {
        let description = &mut self.meta.description;
        section(ui, icon::DOCUMENT, "Description", |ui| {
            ui.add(
                egui::TextEdit::multiline(description)
                    .desired_rows(9)
                    .desired_width(f32::INFINITY)
                    .margin(Margin::same(10)),
            );
            let words = description.split_whitespace().count();
            ui.label(
                muted(
                    ui,
                    format!("{words} words · written to the m4b and to desc.txt"),
                )
                .size(12.0),
            );
        });
    }

    fn chapter_editor(&mut self, ui: &mut Ui) {
        let durations = self.source.durations_ms();
        let total_ms = self.total_ms();
        let audible = self
            .matched
            .as_ref()
            .and_then(|found| found.chapters.clone());
        card(ui, |ui| {
            let p = palette(ui);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::LIST).size(16.0).color(p.accent));
                ui.label(RichText::new("Chapters").text_style(TextStyle::Heading));
                let (color, glyph) = match self.chapter_origin {
                    ChapterOrigin::AudibleTimings => (p.success, icon::CHECK),
                    ChapterOrigin::AudibleTitles => (p.accent, icon::INFO),
                    ChapterOrigin::PerFile => (p.muted, icon::AUDIO),
                };
                chip(ui, glyph, self.chapter_origin.describe(), color);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Chapter 1, 2, 3…").on_hover_text("Rename every chapter to “Chapter N”").clicked() {
                        for (i, row) in self.chapters.iter_mut().enumerate() {
                            row.title = format!("Chapter {}", i + 1);
                        }
                    }
                    if ui.button("One per file").clicked() {
                        self.set_chapters(chapters::per_file(&durations), ChapterOrigin::PerFile);
                    }
                    let reset = ui
                        .add_enabled(audible.is_some(), egui::Button::new(format!("{}  Audible chapters", icon::REFRESH)))
                        .on_hover_text("Audible's timings if the length matches, otherwise its titles if the count matches")
                        .on_disabled_hover_text("Audible has no chapter data for this book");
                    if reset.clicked() {
                        let (chapters, origin) = chapters::best(&durations, audible.as_ref());
                        self.set_chapters(chapters, origin);
                    }
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                column_header(ui, "#", 30.0);
                column_header(ui, "Start", 118.0);
                column_header(ui, "Length", 82.0);
                column_header(ui, "Title", 200.0);
            });
            let starts: Vec<Option<u64>> = self
                .chapters
                .iter()
                .map(|row| chapters::parse_ms(&row.start))
                .collect();
            let mut insert_after = None;
            let mut remove = None;
            let rows = self.chapters.len();
            let row_width = ui.available_width();
            egui::ScrollArea::vertical()
                .id_salt("chapter_rows")
                .max_height(560.0)
                .auto_shrink([false, true])
                .show_rows(ui, CHAPTER_ROW_HEIGHT, rows, |ui, range| {
                    for i in range {
                        let length = match (
                            starts[i],
                            starts.get(i + 1).copied().unwrap_or(Some(total_ms)),
                        ) {
                            (Some(start), Some(end)) if end > start => format_short(end - start),
                            _ => "–".to_owned(),
                        };
                        let action = chapter_row(
                            ui,
                            row_width,
                            i,
                            &mut self.chapters[i],
                            starts[i].is_some(),
                            &length,
                        );
                        match action {
                            RowAction::Insert => insert_after = Some(i),
                            RowAction::Remove => remove = Some(i),
                            RowAction::None => {}
                        }
                    }
                });
            if let Some(i) = insert_after {
                let start = self.chapters[i].start.clone();
                self.chapters.insert(
                    i + 1,
                    ChapterRow {
                        start,
                        title: "New chapter".to_owned(),
                    },
                );
            }
            if let Some(i) = remove {
                self.chapters.remove(i);
            }
            if let Err(problem) = self.parsed_chapters() {
                ui.add_space(4.0);
                ui.colored_label(p.danger, format!("{}  {problem}", icon::WARNING));
            }
        });
    }

    fn audible_tab(&mut self, ui: &mut Ui, settings: &Settings) {
        let regions = settings.regions.clone();
        section(ui, icon::SEARCH, "Find this book on Audible", |ui| {
            let mut search = false;
            ui.horizontal_top(|ui| {
                let width = ui.available_width() - 120.0;
                ui.vertical(|ui| {
                    ui.set_width(width * 0.58);
                    search |= text_field(ui, "Title", &mut self.search_title, "").lost_focus()
                        && enter_pressed(ui);
                });
                ui.vertical(|ui| {
                    ui.set_width(width * 0.42);
                    search |= text_field(ui, "Author", &mut self.search_author, "").lost_focus()
                        && enter_pressed(ui);
                });
                ui.vertical(|ui| {
                    ui.add_space(19.0);
                    search |= ui
                        .add(
                            primary_button(ui, format!("{}  Search", icon::SEARCH))
                                .min_size(egui::vec2(104.0, 32.0)),
                        )
                        .clicked();
                });
            });
            let mut fetch = false;
            ui.horizontal(|ui| {
                ui.label(muted(ui, "Or look up an ASIN"));
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.asin_input)
                        .hint_text("B0…")
                        .desired_width(130.0)
                        .margin(Margin::symmetric(8, 5)),
                );
                fetch |= input.lost_focus() && enter_pressed(ui);
                fetch |= ui.button("Fetch").clicked();
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let names: Vec<&str> = regions.iter().map(|r| r.label()).collect();
                    ui.label(
                        muted(
                            ui,
                            format!("{}  Searching {}", icon::GLOBE, names.join(", ")),
                        )
                        .size(12.5),
                    );
                });
            });
            if search {
                self.search(ui.ctx(), &regions);
            } else if fetch {
                self.fetch(ui.ctx(), &regions);
            }
            if self.lookup.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(muted(ui, "Asking Audible…"));
                });
            }
            if let Some(error) = &self.lookup_error {
                ui.colored_label(palette(ui).danger, format!("{}  {error}", icon::ERROR));
            }
        });
        ui.add_space(6.0);

        if self.hits.is_empty() {
            if self.lookup.is_none() {
                card(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(icon::SEARCH)
                                .size(28.0)
                                .color(palette(ui).muted),
                        );
                        let text = if self.searched {
                            "No results yet"
                        } else {
                            "Search to see matching books"
                        };
                        ui.label(muted(ui, text));
                        ui.add_space(10.0);
                    });
                });
            }
            return;
        }
        let local_secs = self.source.total_secs();
        let in_use = self
            .matched
            .as_ref()
            .map(|found| found.product.asin.clone());
        let mut picked = None;
        let hits = &self.hits;
        let wide = ui.available_width() > 860.0;
        let per_row = if wide { 2 } else { 1 };
        for row in hits.chunks(per_row) {
            ui.horizontal_top(|ui| {
                let width = (ui.available_width() - 14.0 * (per_row - 1) as f32) / per_row as f32;
                ui.spacing_mut().item_spacing.x = 14.0;
                for hit in row {
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        if search_hit(
                            ui,
                            hit,
                            local_secs,
                            in_use.as_deref() == Some(hit.asin.as_str()),
                        ) {
                            picked = Some((hit.asin.clone(), hit.region));
                        }
                    });
                }
            });
            ui.add_space(6.0);
        }
        if let Some((asin, region)) = picked {
            self.use_hit(ui.ctx(), asin, region, &regions);
        }
    }

    fn source_tab(&mut self, ui: &mut Ui) {
        let source = &self.source;
        section(ui, icon::FOLDER, "Folder", |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(source.dir.display().to_string()).monospace());
                if icon_button(ui, icon::OPEN_EXTERNAL, "Open in Explorer").clicked() {
                    open_in_explorer(&source.dir);
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("source_facts")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    let mut fact = |label: &str, value: String| {
                        ui.label(muted(ui, label));
                        ui.label(value);
                        ui.end_row();
                    };
                    fact("Audio", source.format_summary());
                    fact("Length", format_duration(source.total_secs()));
                    fact(
                        "NFO",
                        match &source.nfo {
                            Some(nfo) => {
                                let mut facts = vec!["found".to_owned()];
                                facts.extend(nfo.chapters.map(|count| format!("{count} chapters")));
                                facts.extend(nfo.duration_secs.map(format_duration));
                                facts.join(" · ")
                            }
                            None => "none".to_owned(),
                        },
                    );
                    let size = source
                        .cover
                        .as_ref()
                        .map(|cover| format!(" · {}", cover.describe_size()));
                    fact(
                        "Cover",
                        format!("{}{}", source.cover_origin, size.unwrap_or_default()),
                    );
                    fact(
                        "ASIN",
                        match &source.asin {
                            Some((asin, Some(region))) => {
                                format!("{asin} · audible.{}", region.domain())
                            }
                            Some((asin, None)) => asin.clone(),
                            None => "not in the tags, NFO or folder name".to_owned(),
                        },
                    );
                });
        });
        ui.add_space(6.0);
        section(ui, icon::AUDIO, "Files", |ui| {
            let rows = source.files.len();
            egui::ScrollArea::vertical()
                .id_salt("source_files")
                .max_height(360.0)
                .auto_shrink([false, true])
                .show_rows(ui, 22.0, rows, |ui, range| {
                    for i in range {
                        let file = &source.files[i];
                        let name = file
                            .path
                            .strip_prefix(&source.dir)
                            .unwrap_or(&file.path)
                            .display()
                            .to_string();
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [34.0, 20.0],
                                egui::Label::new(muted(ui, (i + 1).to_string())),
                            );
                            ui.add_sized(
                                [ui.available_width() - 190.0, 20.0],
                                egui::Label::new(name).truncate(),
                            );
                            ui.add_sized(
                                [90.0, 20.0],
                                egui::Label::new(format_short(
                                    (file.probe.duration * 1000.0) as u64,
                                )),
                            );
                            ui.add_sized(
                                [80.0, 20.0],
                                egui::Label::new(muted(
                                    ui,
                                    format!("{} kbps", file.probe.bit_rate / 1000),
                                )),
                            );
                        });
                    }
                });
        });
        if let Some(nfo) = &source.nfo {
            ui.add_space(6.0);
            section(ui, icon::DOCUMENT, "NFO", |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("nfo")
                    .max_height(320.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.label(RichText::new(nfo.raw.trim()).monospace().size(12.5));
                    });
            });
        }
    }

    /// Destination, encoding options and the build button, pinned to the bottom of the window.
    pub fn action_bar(&mut self, ui: &mut Ui, tools: Option<Arc<Tools>>, settings: &Settings) {
        let names = OutputNames::new(Path::new(&settings.library_root), &self.meta);
        self.update_output_check(ui.ctx(), &names);
        let conflicts = self.conflicts(&names);
        centered(ui, CONTENT_WIDTH, |ui| {
            ui.add_space(12.0);
            let p = palette(ui);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::LIBRARY).color(p.muted));
                let full = names.m4b();
                let relative = full.strip_prefix(&settings.library_root).unwrap_or(&full);
                let root = settings.library_root.trim_end_matches('\\');
                ui.add(
                    egui::Label::new(muted(ui, format!("{root}\\")))
                        .truncate()
                        .show_tooltip_when_elided(true),
                );
                ui.label(RichText::new(relative.display().to_string()).strong());
                if icon_button(ui, icon::COPY, "Copy the full path").clicked() {
                    ui.ctx().copy_text(full.display().to_string());
                }
            });
            if let Some(files) = self
                .output_check
                .existing
                .as_ref()
                .filter(|files| !files.is_empty())
            {
                ui.label(
                    RichText::new(format!(
                        "{}  This folder already has: {}",
                        icon::INFO,
                        files.join(", ")
                    ))
                    .size(12.5)
                    .color(p.warning),
                );
            }
            ui.add_space(6.0);
            match &self.build {
                Some(build) => progress_row(ui, build, self.source.total_secs()),
                None => self.ready_row(ui, tools, settings, names, &conflicts),
            }
            self.outcome_row(ui);
            ui.add_space(12.0);
        });
    }

    fn ready_row(
        &mut self,
        ui: &mut Ui,
        tools: Option<Arc<Tools>>,
        settings: &Settings,
        names: OutputNames,
        conflicts: &[String],
    ) {
        let blocker = self.build_blocker(tools.is_some(), conflicts);
        let mut build = false;
        ui.horizontal(|ui| {
            ui.label(muted(ui, "AAC"));
            egui::ComboBox::from_id_salt("bitrate")
                .width(96.0)
                .selected_text(format!("{} kbps", self.bitrate_k))
                .show_ui(ui, |ui| {
                    for &rate in convert::aac_bitrates() {
                        ui.selectable_value(&mut self.bitrate_k, rate, format!("{rate} kbps"));
                    }
                });
            if self.source.max_channels() > 1 {
                egui::ComboBox::from_id_salt("channels")
                    .width(90.0)
                    .selected_text(if self.channels == 1 { "Mono" } else { "Stereo" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.channels, 2, "Stereo");
                        ui.selectable_value(&mut self.channels, 1, "Mono");
                    });
            } else {
                ui.label("Mono");
            }
            ui.label(
                muted(
                    ui,
                    format!(
                        "≈ {}  ·  {} kHz  ·  {} parallel encoders",
                        format_bytes(self.estimated_bytes()),
                        self.source.sample_rate() as f32 / 1000.0,
                        settings.worker_count()
                    ),
                )
                .size(13.0),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let button = primary_button(ui, format!("{}   Build m4b", icon::PLAY))
                    .min_size(egui::vec2(168.0, 38.0));
                let response = ui
                    .add_enabled(blocker.is_none(), button)
                    .on_hover_text("Ctrl+Enter");
                build = response.clicked();
                if let Some(blocker) = &blocker {
                    response.on_disabled_hover_text(blocker);
                }
                if !conflicts.is_empty() {
                    ui.checkbox(
                        &mut self.overwrite,
                        RichText::new("Replace").color(palette(ui).warning),
                    )
                    .on_hover_text(format!("Overwrite {}", conflicts.join(", ")));
                }
                if let Some(blocker) = &blocker {
                    ui.add(egui::Label::new(muted(ui, blocker).size(13.0)).truncate());
                }
            });
        });
        build |= blocker.is_none()
            && ui.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::Enter));
        if build
            && blocker.is_none()
            && let Some(tools) = tools
        {
            self.start_build(ui.ctx(), tools, names, settings);
        }
    }

    fn outcome_row(&mut self, ui: &mut Ui) {
        let p = palette(ui);
        let mut dismiss = false;
        match &self.outcome {
            Some(Outcome::Saved { path, took }) => {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.colored_label(
                        p.success,
                        format!(
                            "{}  Saved to your library in {}",
                            icon::CHECK,
                            format_elapsed(*took)
                        ),
                    );
                    if ui
                        .button(format!("{}  Show in Explorer", icon::OPEN_EXTERNAL))
                        .clicked()
                    {
                        show_in_explorer(path);
                    }
                    dismiss = icon_button(ui, icon::CLOSE, "Dismiss").clicked();
                });
            }
            Some(Outcome::Failed(error)) => {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("{}  {error}", icon::ERROR)).color(p.danger),
                        )
                        .wrap(),
                    );
                    dismiss = icon_button(ui, icon::CLOSE, "Dismiss").clicked();
                });
            }
            Some(Outcome::Cancelled) => {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(muted(
                        ui,
                        "Build cancelled. Nothing was written to the library.",
                    ));
                    dismiss = icon_button(ui, icon::CLOSE, "Dismiss").clicked();
                });
            }
            None => {}
        }
        if dismiss {
            self.outcome = None;
        }
    }
}

fn progress_row(ui: &mut Ui, build: &Build, total_secs: f64) {
    let p = palette(ui);
    let (stage, fraction) = build.progress();
    let index = Stage::ALL.iter().position(|&s| s == stage).unwrap_or(0);
    let overall: f32 = STAGE_WEIGHTS[..index].iter().sum::<f32>() + STAGE_WEIGHTS[index] * fraction;
    ui.horizontal(|ui| {
        for (i, step) in Stage::ALL.iter().enumerate() {
            if i > 0 {
                ui.label(muted(ui, "—"));
            }
            if i < index {
                ui.colored_label(p.success, format!("{}  {}", icon::CHECK, step.label()));
            } else if i == index {
                ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                ui.label(
                    RichText::new(step.label())
                        .color(p.accent)
                        .family(FontFamily::Name(SEMIBOLD.into())),
                );
            } else {
                ui.label(muted(ui, step.label()));
            }
        }
    });
    ui.horizontal(|ui| {
        progress_bar(ui, overall, (ui.available_width() - 120.0).max(100.0), 10.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if build.is_cancelling() {
                "Stopping…"
            } else {
                "Cancel"
            };
            if ui
                .add_enabled(
                    !build.is_cancelling(),
                    egui::Button::new(label).min_size(egui::vec2(96.0, 32.0)),
                )
                .clicked()
            {
                build.cancel();
            }
        });
    });
    let mut facts = vec![format!("{:.0}%", overall * 100.0)];
    let stage_secs = build.stage_started.elapsed().as_secs_f64();
    if stage == Stage::Encoding && stage_secs > 0.5 && fraction > 0.0 {
        let speed = f64::from(fraction) * total_secs / stage_secs;
        facts.push(format!("{}  {:.0}× realtime", icon::LIGHTNING, speed));
    }
    let elapsed = build.started.elapsed().as_secs_f64();
    if overall > 0.04 {
        let left = elapsed / f64::from(overall) - elapsed;
        facts.push(format!(
            "about {} left",
            format_elapsed(Duration::from_secs_f64(left.max(1.0)))
        ));
    }
    facts.push(format!("{} encoders", build.workers));
    ui.label(muted(ui, facts.join("   ·   ")).size(13.0));
    ui.ctx().request_repaint_after(Duration::from_millis(250));
}

enum RowAction {
    None,
    Insert,
    Remove,
}

fn chapter_row(
    ui: &mut Ui,
    width: f32,
    index: usize,
    row: &mut ChapterRow,
    valid: bool,
    length: &str,
) -> RowAction {
    let p = palette(ui);
    let mut action = RowAction::None;
    let fill = if index % 2 == 1 {
        ui.visuals().faint_bg_color
    } else {
        egui::Color32::TRANSPARENT
    };
    egui::Frame::new()
        .fill(fill)
        .corner_radius(6)
        .inner_margin(Margin::symmetric(8, 1))
        .show(ui, |ui| {
            ui.set_width(width - 16.0);
            ui.horizontal(|ui| {
                ui.set_height(CHAPTER_ROW_HEIGHT - 2.0);
                ui.add_sized(
                    [30.0, 24.0],
                    egui::Label::new(muted(ui, (index + 1).to_string())),
                );
                let start = egui::TextEdit::singleline(&mut row.start)
                    .font(TextStyle::Monospace)
                    .desired_width(110.0)
                    .margin(Margin::symmetric(6, 3));
                ui.add(if valid {
                    start
                } else {
                    start.text_color(p.danger)
                });
                ui.add_sized([82.0, 24.0], egui::Label::new(muted(ui, length)));
                ui.add(
                    egui::TextEdit::singleline(&mut row.title)
                        .desired_width((ui.available_width() - 92.0).max(120.0))
                        .margin(Margin::symmetric(6, 3)),
                );
                if icon_button(ui, icon::ADD, "Insert a chapter below").clicked() {
                    action = RowAction::Insert;
                }
                if icon_button(ui, icon::DELETE, "Remove this chapter").clicked() {
                    action = RowAction::Remove;
                }
            });
        });
    action
}

/// A search result. Returns true when "Use this" was clicked.
fn search_hit(ui: &mut Ui, hit: &Product, local_secs: f64, in_use: bool) -> bool {
    let mut clicked = false;
    card(ui, |ui| {
        let p = palette(ui);
        ui.horizontal_top(|ui| {
            let thumb = Vec2::splat(92.0);
            match &hit.thumb_url {
                Some(url) => {
                    ui.add(
                        egui::Image::new(url.as_str())
                            .fit_to_exact_size(thumb)
                            .corner_radius(6),
                    );
                }
                None => {
                    let (rect, _) = ui.allocate_exact_size(thumb, Sense::hover());
                    ui.painter().rect_filled(rect, 6.0, p.raised);
                }
            }
            ui.vertical(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(&hit.title)
                            .size(15.5)
                            .family(FontFamily::Name(SEMIBOLD.into())),
                    )
                    .wrap(),
                );
                if !hit.subtitle.is_empty() {
                    ui.add(egui::Label::new(muted(ui, &hit.subtitle).size(12.5)).wrap());
                }
                ui.label(format!("by {}", hit.authors.join(", ")));
                if !hit.narrators.is_empty() {
                    ui.label(
                        muted(ui, format!("Narrated by {}", hit.narrators.join(", "))).size(12.5),
                    );
                }
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    if let Some(series) = hit.series.first() {
                        chip(
                            ui,
                            icon::LIBRARY,
                            format!("{} #{}", series.name, series.position),
                            p.highlight,
                        );
                    }
                    if let Some(min) = hit.runtime_min {
                        chip(
                            ui,
                            icon::CLOCK,
                            format_duration(f64::from(min) * 60.0),
                            p.muted,
                        );
                    }
                    if let Some(year) = hit.release_date.get(..4) {
                        chip(ui, icon::CALENDAR, year, p.muted);
                    }
                    chip(
                        ui,
                        icon::GLOBE,
                        format!("{} · {}", hit.region.label(), capitalize(&hit.language)),
                        p.muted,
                    );
                });
                let difference = hit
                    .runtime_min
                    .map(|min| (f64::from(min) * 60.0 - local_secs).abs());
                let matches = difference.is_some_and(|d| d <= RUNTIME_MATCH_SECS);
                ui.horizontal(|ui| {
                    match difference {
                        Some(_) if matches => {
                            ui.colored_label(
                                p.success,
                                format!("{}  Same length as your files", icon::CHECK),
                            );
                        }
                        Some(difference) => {
                            ui.label(
                                muted(
                                    ui,
                                    format!("Length differs by {}", format_duration(difference)),
                                )
                                .size(12.5),
                            );
                        }
                        None => {}
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if in_use {
                            chip(ui, icon::CHECK, "In use", p.success);
                        } else if matches {
                            clicked = ui.add(primary_button(ui, "Use this")).clicked();
                        } else {
                            clicked = ui.button("Use this").clicked();
                        }
                    });
                });
            });
        });
    });
    clicked
}

fn cover_view(ui: &mut Ui, cover: Option<&Picture>) {
    let size = Vec2::splat(COVER_SIZE);
    match cover {
        Some(cover) => {
            let image = egui::Image::from_bytes(cover.uri.clone(), cover.bytes.clone())
                .fit_to_exact_size(size)
                .corner_radius(8);
            ui.add_sized(size, image);
        }
        None => {
            let p = palette(ui);
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            ui.painter().rect_filled(rect, 8.0, p.raised);
            ui.painter().text(
                rect.center() - egui::vec2(0.0, 12.0),
                egui::Align2::CENTER_CENTER,
                icon::PICTURE,
                egui::FontId::proportional(34.0),
                p.muted,
            );
            ui.painter().text(
                rect.center() + egui::vec2(0.0, 24.0),
                egui::Align2::CENTER_CENTER,
                "No cover",
                egui::FontId::proportional(13.0),
                p.muted,
            );
        }
    }
}

/// A row of toggle buttons where exactly one is selected.
fn segmented<T: PartialEq + Copy>(ui: &mut Ui, current: &mut T, options: &[(T, String)]) {
    let p = palette(ui);
    egui::Frame::new()
        .fill(p.raised)
        .corner_radius(8)
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (value, label) in options {
                    let selected = *current == *value;
                    let text = RichText::new(label).size(12.5).color(if selected {
                        p.text
                    } else {
                        p.muted
                    });
                    let button = egui::Button::new(text)
                        .fill(if selected {
                            p.surface
                        } else {
                            egui::Color32::TRANSPARENT
                        })
                        .stroke(if selected {
                            Stroke::new(1.0, p.border)
                        } else {
                            Stroke::NONE
                        })
                        .corner_radius(6)
                        .min_size(egui::vec2(0.0, 26.0));
                    if ui.add(button).clicked() {
                        *current = *value;
                    }
                }
            });
        });
}

fn text_field(ui: &mut Ui, label: &str, value: &mut String, hint: &str) -> Response {
    field_label(ui, label);
    let response = ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(hint)
            .desired_width(f32::INFINITY)
            .margin(Margin::symmetric(8, 6)),
    );
    ui.add_space(2.0);
    response
}

fn two_fields(ui: &mut Ui, left: (&str, &mut String, &str), right: (&str, &mut String, &str)) {
    ui.horizontal_top(|ui| {
        let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
        ui.vertical(|ui| {
            ui.set_width(width);
            text_field(ui, left.0, left.1, left.2);
        });
        ui.vertical(|ui| {
            ui.set_width(width);
            text_field(ui, right.0, right.1, right.2);
        });
    });
}

fn column_header(ui: &mut Ui, text: &str, width: f32) {
    ui.add_sized([width, 18.0], egui::Label::new(muted(ui, text).size(12.5)));
}

fn enter_pressed(ui: &Ui) -> bool {
    ui.input(|input| input.key_pressed(Key::Enter))
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// `h:mm:ss`, or `m:ss` below an hour.
fn format_short(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

pub fn format_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min {} s", secs / 60, secs % 60),
        _ => format!("{} h {} min", secs / 3600, secs / 60 % 60),
    }
}

fn format_bytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1000.0)
    } else {
        format!("{mb:.0} MB")
    }
}

pub fn open_in_explorer(dir: &Path) {
    std::process::Command::new("explorer").arg(dir).spawn().ok();
}

fn show_in_explorer(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_times_and_sizes() {
        assert_eq!(format_short(3_723_000), "1:02:03");
        assert_eq!(format_short(83_000), "1:23");
        assert_eq!(format_elapsed(Duration::from_secs(32)), "32 s");
        assert_eq!(format_elapsed(Duration::from_secs(130)), "2 min 10 s");
        assert_eq!(format_bytes(626_000_000), "626 MB");
        assert_eq!(format_bytes(1_460_000_000), "1.5 GB");
        assert_eq!(capitalize("english"), "English");
    }
}
