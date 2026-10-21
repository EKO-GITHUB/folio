//! One opened book folder and everything decided about it so far. The UI lives in `session_ui.rs`.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use eframe::egui;

use crate::{
    audible::{self, AudibleChapters, Product, Region},
    book::BookMeta,
    chapters::{self, Chapter, ChapterOrigin},
    convert::{self, Job, Stage},
    ffmpeg::Tools,
    naming::OutputNames,
    picture::Picture,
    settings::Settings,
    source::SourceBook,
    task::Task,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Details,
    Chapters,
    Audible,
    Source,
}

pub enum Lookup {
    Hits(Vec<Product>),
    Match(Box<AudibleMatch>),
}

pub struct AudibleMatch {
    pub product: Product,
    pub chapters: Option<AudibleChapters>,
    pub cover: Option<Picture>,
}

pub struct ChapterRow {
    pub start: String,
    pub title: String,
}

pub struct Build {
    progress: Arc<Mutex<(Stage, f32)>>,
    cancel: Arc<AtomicBool>,
    task: Task<PathBuf>,
    pub started: Instant,
    pub stage: Stage,
    pub stage_started: Instant,
    pub workers: usize,
}

impl Build {
    pub fn progress(&self) -> (Stage, f32) {
        *self.progress.lock().unwrap()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

pub enum Outcome {
    Saved { path: PathBuf, took: Duration },
    Failed(String),
    Cancelled,
}

/// Something worth a toast.
pub enum Notice {
    Success(String),
    Error(String),
}

/// The files already in the output folder, listed off the UI thread since it's usually on a NAS.
#[derive(Default)]
pub struct OutputCheck {
    pub dir: PathBuf,
    pub existing: Option<Vec<String>>,
    task: Option<Task<Vec<String>>>,
}

pub struct Session {
    pub source: SourceBook,
    pub meta: BookMeta,
    pub tab: Tab,
    pub search_title: String,
    pub search_author: String,
    pub asin_input: String,
    pub lookup: Option<Task<Lookup>>,
    pub lookup_error: Option<String>,
    pub searched: bool,
    pub hits: Vec<Product>,
    pub matched: Option<AudibleMatch>,
    pub chapters: Vec<ChapterRow>,
    pub chapter_origin: ChapterOrigin,
    pub use_local_cover: bool,
    pub bitrate_k: u32,
    pub channels: u32,
    pub overwrite: bool,
    pub new_genre: String,
    pub output_check: OutputCheck,
    pub build: Option<Build>,
    pub outcome: Option<Outcome>,
}

impl Session {
    pub fn new(source: SourceBook) -> Self {
        let meta = BookMeta::from_source(&source);
        let mut session = Self {
            search_title: meta.title.clone(),
            search_author: meta
                .author_list()
                .first()
                .copied()
                .unwrap_or_default()
                .to_owned(),
            asin_input: meta.asin.clone(),
            tab: Tab::Details,
            lookup: None,
            lookup_error: None,
            searched: false,
            hits: Vec::new(),
            matched: None,
            chapters: Vec::new(),
            chapter_origin: ChapterOrigin::PerFile,
            use_local_cover: true,
            bitrate_k: convert::matching_aac_bitrate(source.max_bitrate_k(), source.max_channels()),
            channels: source.max_channels(),
            overwrite: false,
            new_genre: String::new(),
            output_check: OutputCheck::default(),
            build: None,
            outcome: None,
            meta,
            source,
        };
        session.set_chapters(
            chapters::per_file(&session.source.durations_ms()),
            ChapterOrigin::PerFile,
        );
        session
    }

    /// Fetches the book directly when the files name an ASIN, otherwise searches for it.
    pub fn start_lookup(&mut self, ctx: &egui::Context, regions: &[Region]) {
        if self.asin_input.is_empty() {
            self.search(ctx, regions);
        } else {
            self.fetch(ctx, regions);
        }
    }

    pub fn is_building(&self) -> bool {
        self.build.is_some()
    }

    pub fn total_ms(&self) -> u64 {
        self.source.durations_ms().iter().sum()
    }

    pub fn search(&mut self, ctx: &egui::Context, regions: &[Region]) {
        let title = self.search_title.trim().to_owned();
        let author = self.search_author.trim().to_owned();
        if title.is_empty() {
            return;
        }
        let regions = regions.to_vec();
        self.lookup_error = None;
        self.searched = true;
        self.lookup = Some(Task::spawn(ctx, move || {
            audible::search(&regions, &title, &author).map(Lookup::Hits)
        }));
    }

    /// Fetches `asin_input`, trying its known region first, then the configured ones, then the rest.
    pub fn fetch(&mut self, ctx: &egui::Context, regions: &[Region]) {
        let asin = self.asin_input.trim().to_uppercase();
        if asin.is_empty() {
            return;
        }
        let mut order: Vec<Region> = Vec::new();
        for region in self
            .meta
            .region
            .into_iter()
            .chain(regions.iter().copied())
            .chain(Region::ALL)
        {
            if !order.contains(&region) {
                order.push(region);
            }
        }
        self.lookup_error = None;
        self.lookup = Some(Task::spawn(ctx, move || {
            fetch_match(&asin, &order).map(|found| Lookup::Match(Box::new(found)))
        }));
    }

    pub fn use_hit(
        &mut self,
        ctx: &egui::Context,
        asin: String,
        region: Region,
        regions: &[Region],
    ) {
        self.asin_input = asin;
        self.meta.region = Some(region);
        self.fetch(ctx, regions);
    }

    pub fn poll(&mut self) -> Vec<Notice> {
        let mut notices = Vec::new();
        if let Some(task) = &self.lookup
            && let Some(result) = task.poll()
        {
            self.lookup = None;
            match result {
                Ok(Lookup::Hits(hits)) => {
                    if hits.is_empty() {
                        self.lookup_error = Some(
                            "Audible found nothing. Try a shorter title or only the author."
                                .to_owned(),
                        );
                    }
                    self.hits = hits;
                }
                Ok(Lookup::Match(found)) => {
                    notices.push(Notice::Success(format!(
                        "Filled in from Audible {}: {}",
                        found.product.region.label(),
                        found.product.title
                    )));
                    self.apply_match(*found);
                }
                Err(error) => self.lookup_error = Some(format!("{error:#}")),
            }
        }
        if let Some(build) = &mut self.build {
            let (stage, _) = build.progress();
            if stage != build.stage {
                build.stage = stage;
                build.stage_started = Instant::now();
            }
            if let Some(result) = build.task.poll() {
                let took = build.started.elapsed();
                let cancelled = build.is_cancelling();
                self.build = None;
                self.overwrite = false;
                self.output_check = OutputCheck::default();
                self.outcome = Some(match result {
                    Ok(path) => {
                        notices.push(Notice::Success(format!("Saved {}", self.meta.title)));
                        Outcome::Saved { path, took }
                    }
                    Err(_) if cancelled => Outcome::Cancelled,
                    Err(error) => {
                        let message = format!("{error:#}");
                        notices.push(Notice::Error(message.clone()));
                        Outcome::Failed(message)
                    }
                });
            }
        }
        if let Some(task) = &self.output_check.task
            && let Some(result) = task.poll()
        {
            self.output_check.task = None;
            self.output_check.existing = Some(result.unwrap_or_default());
        }
        notices
    }

    pub fn apply_match(&mut self, found: AudibleMatch) {
        self.meta.apply_audible(&found.product);
        self.asin_input = found.product.asin.clone();
        let (chapters, origin) =
            chapters::best(&self.source.durations_ms(), found.chapters.as_ref());
        self.set_chapters(chapters, origin);
        self.use_local_cover = found.cover.is_none();
        self.matched = Some(found);
    }

    pub fn set_chapters(&mut self, chapters: Vec<Chapter>, origin: ChapterOrigin) {
        self.chapters = chapters
            .into_iter()
            .map(|chapter| ChapterRow {
                start: chapters::format_ms(chapter.start_ms),
                title: chapter.title,
            })
            .collect();
        self.chapter_origin = origin;
    }

    pub fn parsed_chapters(&self) -> Result<Vec<Chapter>, String> {
        let total_ms = self.total_ms();
        let mut chapters: Vec<Chapter> = Vec::new();
        for (i, row) in self.chapters.iter().enumerate() {
            let number = i + 1;
            let start_ms = chapters::parse_ms(&row.start)
                .ok_or_else(|| format!("Chapter {number}: “{}” is not a time", row.start))?;
            if i == 0 && start_ms != 0 {
                return Err("The first chapter has to start at 0:00:00".to_owned());
            }
            if start_ms >= total_ms {
                return Err(format!("Chapter {number} starts after the end of the book"));
            }
            if chapters
                .last()
                .is_some_and(|previous| start_ms <= previous.start_ms)
            {
                return Err(format!("Chapter {number} has to start after chapter {i}"));
            }
            if row.title.trim().is_empty() {
                return Err(format!("Chapter {number} has no title"));
            }
            chapters.push(Chapter {
                start_ms,
                title: row.title.trim().to_owned(),
            });
        }
        if chapters.is_empty() {
            return Err("Add at least one chapter".to_owned());
        }
        Ok(chapters)
    }

    pub fn audible_cover(&self) -> Option<&Picture> {
        self.matched.as_ref().and_then(|found| found.cover.as_ref())
    }

    pub fn cover(&self) -> Option<&Picture> {
        let audible = self.audible_cover();
        let local = self.source.cover.as_ref();
        if self.use_local_cover {
            local.or(audible)
        } else {
            audible.or(local)
        }
    }

    /// Genres are stored like Audible's category ladders: `A / B;A / C`.
    pub fn genres(&self) -> Vec<String> {
        self.meta
            .genre
            .split(';')
            .map(str::trim)
            .filter(|genre| !genre.is_empty())
            .map(str::to_owned)
            .collect()
    }

    pub fn set_genres(&mut self, genres: &[String]) {
        self.meta.genre = genres.join(";");
    }

    pub fn estimated_bytes(&self) -> u64 {
        let audio = u64::from(self.bitrate_k) * 1000 / 8 * self.total_ms() / 1000;
        audio + self.cover().map_or(0, |cover| cover.bytes.len() as u64)
    }

    pub fn update_output_check(&mut self, ctx: &egui::Context, names: &OutputNames) {
        if self.output_check.dir != names.dir {
            let dir = names.dir.clone();
            let task = Task::spawn(ctx, move || list_files(&dir));
            self.output_check = OutputCheck {
                dir: names.dir.clone(),
                existing: None,
                task: Some(task),
            };
        }
    }

    /// Existing files the build would replace.
    pub fn conflicts(&self, names: &OutputNames) -> Vec<String> {
        let ours = [
            names.m4b(),
            names.cover("jpg"),
            names.cover("png"),
            names.description(),
            names.reader(),
        ];
        let existing = self.output_check.existing.as_deref().unwrap_or_default();
        ours.iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| existing.iter().any(|file| file.eq_ignore_ascii_case(name)))
            .collect()
    }

    /// Why the book can't be built yet, if anything stops it.
    pub fn build_blocker(&self, tools_ready: bool, conflicts: &[String]) -> Option<String> {
        if !tools_ready {
            return Some("Waiting for ffmpeg".to_owned());
        }
        if self.lookup.is_some() {
            return Some("Waiting for Audible".to_owned());
        }
        if self.meta.title.trim().is_empty() {
            return Some("Enter a title".to_owned());
        }
        if self.meta.author_list().is_empty() {
            return Some("Enter an author".to_owned());
        }
        if let Err(problem) = self.parsed_chapters() {
            return Some(problem);
        }
        if self.output_check.existing.is_none() {
            return Some("Checking the library folder…".to_owned());
        }
        if !conflicts.is_empty() && !self.overwrite {
            return Some(
                "Files with these names exist. Tick “Replace” to overwrite them.".to_owned(),
            );
        }
        None
    }

    pub fn start_build(
        &mut self,
        ctx: &egui::Context,
        tools: Arc<Tools>,
        names: OutputNames,
        settings: &Settings,
    ) {
        let chapters = match self.parsed_chapters() {
            Ok(chapters) => chapters,
            Err(problem) => {
                self.outcome = Some(Outcome::Failed(problem));
                return;
            }
        };
        let workers = settings.worker_count();
        let job = Job {
            files: self
                .source
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect(),
            durations_ms: self.source.durations_ms(),
            chapters,
            meta: self.meta.clone(),
            cover: self.cover().cloned(),
            bitrate_k: self.bitrate_k,
            channels: self.channels,
            sample_rate: self.source.sample_rate(),
            workers,
            low_priority: settings.low_priority,
            library_root: PathBuf::from(&settings.library_root),
            names,
        };
        let progress = Arc::new(Mutex::new((Stage::Measuring, 0.0)));
        let cancel = Arc::new(AtomicBool::new(false));
        let shared = progress.clone();
        let stop = cancel.clone();
        let repaint = ctx.clone();
        let task = Task::spawn(ctx, move || {
            let report = |stage, fraction| {
                *shared.lock().unwrap() = (stage, fraction);
                repaint.request_repaint();
            };
            convert::run(&tools, &job, &report, &stop)
        });
        let now = Instant::now();
        self.outcome = None;
        self.build = Some(Build {
            progress,
            cancel,
            task,
            started: now,
            stage: Stage::Measuring,
            stage_started: now,
            workers,
        });
    }
}

#[cfg(test)]
impl Session {
    /// Shows a build in progress without running one.
    pub fn preview_build(&mut self, ctx: &egui::Context, stage: Stage, fraction: f32) {
        let task = Task::spawn(ctx, || {
            std::thread::sleep(Duration::from_secs(3600));
            anyhow::bail!("preview")
        });
        let started = Instant::now() - Duration::from_secs(18);
        self.build = Some(Build {
            progress: Arc::new(Mutex::new((stage, fraction))),
            cancel: Arc::new(AtomicBool::new(false)),
            task,
            started,
            stage,
            stage_started: started + Duration::from_secs(4),
            workers: 32,
        });
    }
}

fn fetch_match(asin: &str, regions: &[Region]) -> anyhow::Result<AudibleMatch> {
    let product = regions
        .iter()
        .find_map(|&region| audible::product(asin, region).ok())
        .with_context(|| format!("{asin} was not found on Audible"))?;
    let chapters = audible::chapters(asin, product.region).ok();
    let cover = product
        .cover_url
        .as_deref()
        .and_then(|url| audible::download(url).ok())
        .map(Picture::new);
    Ok(AudibleMatch {
        product,
        chapters,
        cover,
    })
}

fn list_files(dir: &Path) -> anyhow::Result<Vec<String>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}
