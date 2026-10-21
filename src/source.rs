use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use anyhow::{Result, ensure};
use regex::Regex;

use crate::{
    audible::Region,
    ffmpeg::{self, Probe, Tools},
    nfo::{self, Nfo},
    picture::Picture,
};

const AUDIO_EXTENSIONS: [&str; 8] = ["mp3", "m4a", "m4b", "aac", "flac", "ogg", "opus", "wav"];
const IMAGE_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];
const PROBE_THREADS: usize = 8;

static AUDIBLE_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)audible\.([a-z.]+?)/pd/(?:[^/\s]+/)?([A-Z0-9]{10})\b").unwrap()
});
static ASIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(B0[0-9A-Z]{8})\b").unwrap());

pub struct SourceFile {
    pub path: PathBuf,
    pub probe: Probe,
}

/// Everything found in a book folder: the audio files in playback order plus NFO, cover and ASIN.
pub struct SourceBook {
    pub dir: PathBuf,
    pub files: Vec<SourceFile>,
    pub nfo: Option<Nfo>,
    pub cover: Option<Picture>,
    pub cover_origin: String,
    pub asin: Option<(String, Option<Region>)>,
}

impl SourceBook {
    pub fn total_secs(&self) -> f64 {
        self.files.iter().map(|file| file.probe.duration).sum()
    }

    pub fn durations_ms(&self) -> Vec<u64> {
        self.files
            .iter()
            .map(|file| (file.probe.duration * 1000.0).round() as u64)
            .collect()
    }

    /// A tag from the first file that has it.
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.files
            .iter()
            .find_map(|file| file.probe.tags.get(key))
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
    }

    pub fn max_bitrate_k(&self) -> u32 {
        self.files
            .iter()
            .map(|file| file.probe.bit_rate / 1000)
            .max()
            .unwrap_or(128)
    }

    pub fn max_channels(&self) -> u32 {
        self.files
            .iter()
            .map(|file| file.probe.channels)
            .max()
            .unwrap_or(2)
            .clamp(1, 2)
    }

    pub fn sample_rate(&self) -> u32 {
        self.files
            .iter()
            .map(|file| file.probe.sample_rate)
            .max()
            .unwrap_or(44100)
    }

    pub fn format_summary(&self) -> String {
        let codec = self.files[0].probe.codec.to_uppercase();
        let channels = if self.max_channels() == 1 {
            "mono"
        } else {
            "stereo"
        };
        format!(
            "{} × {codec}, {} kbps {channels}, {:.1} kHz",
            self.files.len(),
            self.max_bitrate_k(),
            self.sample_rate() as f32 / 1000.0
        )
    }
}

pub fn scan(
    dir: &Path,
    tools: &Tools,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<SourceBook> {
    let mut audio = Vec::new();
    let mut images = Vec::new();
    let mut nfos = Vec::new();
    walk(dir, &mut |path| {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
            audio.push(path);
        } else if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            images.push(path);
        } else if ext == "nfo" {
            nfos.push(path);
        }
    })?;
    ensure!(
        !audio.is_empty(),
        "No audio files found in {}",
        dir.display()
    );
    audio.sort_by(|a, b| natord::compare_ignore_case(&a.to_string_lossy(), &b.to_string_lossy()));

    let probes = probe_all(tools, &audio, progress)?;
    let files: Vec<SourceFile> = audio
        .into_iter()
        .zip(probes)
        .map(|(path, probe)| SourceFile { path, probe })
        .collect();
    let nfo = nfos.first().and_then(|path| nfo::read(path).ok());
    let (cover, cover_origin) = find_cover(tools, &images, &files);
    let asin = detect_asin(&files, nfo.as_ref(), dir);
    Ok(SourceBook {
        dir: dir.to_path_buf(),
        files,
        nfo,
        cover,
        cover_origin,
        asin,
    })
}

fn walk(dir: &Path, visit: &mut dyn FnMut(PathBuf)) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk(&path, visit)?;
        } else {
            visit(path);
        }
    }
    Ok(())
}

fn probe_all(
    tools: &Tools,
    paths: &[PathBuf],
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Vec<Probe>> {
    let done = AtomicUsize::new(0);
    let chunk_size = paths.len().div_ceil(PROBE_THREADS);
    thread::scope(|scope| {
        let workers: Vec<_> = paths
            .chunks(chunk_size)
            .map(|chunk| {
                let done = &done;
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|path| {
                            let probe = ffmpeg::probe(tools, path);
                            progress(done.fetch_add(1, Ordering::Relaxed) + 1, paths.len());
                            probe
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("probe thread panicked"))
            .collect()
    })
}

/// Prefers an image called cover/folder, then the largest image, then art embedded in the audio.
fn find_cover(
    tools: &Tools,
    images: &[PathBuf],
    files: &[SourceFile],
) -> (Option<Picture>, String) {
    let named = images.iter().find(|path| {
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        stem.contains("cover") || stem.contains("folder")
    });
    let largest = || {
        images
            .iter()
            .max_by_key(|path| fs::metadata(path).map_or(0, |m| m.len()))
    };
    if let Some(path) = named.or_else(largest)
        && let Ok(bytes) = fs::read(path)
    {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        return (Some(Picture::new(bytes)), name);
    }
    let embedded = files
        .iter()
        .filter(|file| file.probe.has_picture)
        .find_map(|file| ffmpeg::extract_picture(tools, &file.path).ok());
    match embedded {
        Some(bytes) => (Some(Picture::new(bytes)), "embedded in audio".to_owned()),
        None => (None, "none".to_owned()),
    }
}

fn detect_asin(
    files: &[SourceFile],
    nfo: Option<&Nfo>,
    dir: &Path,
) -> Option<(String, Option<Region>)> {
    let tags = files.first().map(|file| &file.probe.tags);
    let tag_values = tags.into_iter().flat_map(|tags| tags.values());
    let texts = tag_values
        .map(String::as_str)
        .chain(nfo.map(|nfo| nfo.raw.as_str()));
    for text in texts.clone() {
        if let Some(caps) = AUDIBLE_URL.captures(text) {
            return Some((caps[2].to_uppercase(), Region::from_domain(&caps[1])));
        }
    }
    let explicit = tags.and_then(|tags| tags.get("asin").or_else(|| tags.get("audible_asin")));
    let dir_name = dir.file_name().unwrap_or_default().to_string_lossy();
    explicit
        .map(String::as_str)
        .into_iter()
        .chain(texts)
        .chain([dir_name.as_ref()])
        .find_map(|text| ASIN.captures(text).map(|caps| (caps[1].to_owned(), None)))
}
