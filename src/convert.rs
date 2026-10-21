use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use mp4ameta::{Data, Fourcc, FreeformIdent, Img, MediaType, Userdata, WriteConfig};

use crate::{
    book::BookMeta,
    chapters::Chapter,
    encode::{self, PRIMING, PcmFormat},
    faststart,
    ffmpeg::{self, Tools},
    mp4,
    naming::OutputNames,
    picture::Picture,
};

const AAC_BITRATES: [u32; 8] = [32, 48, 64, 80, 96, 128, 160, 192];
/// The encoded file may differ this much from the sum of its sources before it's treated as broken.
const DURATION_TOLERANCE_SECS: f64 = 2.0;

pub fn aac_bitrates() -> &'static [u32] {
    &AAC_BITRATES
}

/// AAC needs roughly three quarters of the MP3 bitrate for the same quality.
pub fn matching_aac_bitrate(source_kbps: u32, channels: u32) -> u32 {
    let target = source_kbps * 3 / 4;
    let max = if channels == 1 { 128 } else { 192 };
    AAC_BITRATES
        .into_iter()
        .filter(|&rate| rate <= max)
        .min_by_key(|&rate| (rate.abs_diff(target), u32::MAX - rate))
        .unwrap_or(64)
}

pub struct Job {
    pub files: Vec<PathBuf>,
    /// The probed length of each file. Chapter starts are on this timeline.
    pub durations_ms: Vec<u64>,
    pub chapters: Vec<Chapter>,
    pub meta: BookMeta,
    pub cover: Option<Picture>,
    pub bitrate_k: u32,
    pub channels: u32,
    pub sample_rate: u32,
    pub workers: usize,
    pub low_priority: bool,
    pub library_root: PathBuf,
    pub names: OutputNames,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Measuring,
    Encoding,
    Finishing,
    Copying,
}

impl Stage {
    pub const ALL: [Stage; 4] = [
        Stage::Measuring,
        Stage::Encoding,
        Stage::Finishing,
        Stage::Copying,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Stage::Measuring => "Reading audio",
            Stage::Encoding => "Encoding",
            Stage::Finishing => "Chapters & tags",
            Stage::Copying => "Copying to library",
        }
    }
}

/// Encodes and tags the book in a local temp folder, then copies it into the library.
pub fn run(
    tools: &Tools,
    job: &Job,
    report: &(dyn Fn(Stage, f32) + Sync),
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    // Creates a missing library folder, e.g. the default one on first use, but only if its
    // parent exists, so an offline share still reports that it can't be reached.
    if !job.library_root.is_dir() && job.library_root.parent().is_some_and(Path::is_dir) {
        fs::create_dir(&job.library_root).with_context(|| {
            format!(
                "Could not create the library folder {}",
                job.library_root.display()
            )
        })?;
    }
    ensure!(
        job.library_root.is_dir(),
        "The library folder {} can't be reached",
        job.library_root.display()
    );
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let work = std::env::temp_dir()
        .join("folio")
        .join(format!("{}-{stamp}", std::process::id()));
    fs::create_dir_all(&work)?;
    let result = run_in(&work, tools, job, report, cancel);
    fs::remove_dir_all(&work).ok();
    result
}

fn run_in(
    work: &Path,
    tools: &Tools,
    job: &Job,
    report: &(dyn Fn(Stage, f32) + Sync),
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    let settings = encode::Settings {
        format: PcmFormat {
            sample_rate: job.sample_rate,
            channels: job.channels,
        },
        bitrate_k: job.bitrate_k,
        workers: job.workers.max(1),
        low_priority: job.low_priority,
    };
    let rate = u64::from(job.sample_rate);

    report(Stage::Measuring, 0.0);
    let measurement = encode::measure(tools, &job.files, &settings, cancel, &|fraction| {
        report(Stage::Measuring, fraction)
    })?;
    let total = measurement.total();
    ensure!(total > 0, "The audio files contain no audio");
    let chapter_starts: Vec<u64> = job
        .chapters
        .iter()
        .map(|chapter| {
            exact_position(
                chapter.start_ms,
                &job.durations_ms,
                &measurement.lengths,
                rate,
            )
        })
        .collect();

    report(Stage::Encoding, 0.0);
    let segments = measurement.segments(settings.workers, job.sample_rate);
    let encoded = work.join("book.m4b");
    let mut writer = mp4::Writer::create(&encoded)?;
    encode::encode(
        tools,
        &job.files,
        &measurement,
        &segments,
        &settings,
        work,
        &mut |frame| writer.push_adts(frame),
        cancel,
        &|fraction| report(Stage::Encoding, fraction),
    )?;

    report(Stage::Finishing, 0.0);
    writer.finish(PRIMING, total)?;
    let chapters: Vec<Chapter> = job
        .chapters
        .iter()
        .zip(&chapter_starts)
        .map(|(chapter, &start)| Chapter {
            start_ms: start * 1000 / rate,
            title: chapter.title.clone(),
        })
        .collect();
    write_tags(&encoded, job, &chapters)?;
    let duration = ffmpeg::probe(tools, &encoded)?.duration;
    let expected = total as f64 / rate as f64;
    ensure!(
        (duration - expected).abs() <= DURATION_TOLERANCE_SECS,
        "The encoded book is {duration:.1}s long, expected {expected:.1}s"
    );

    report(Stage::Copying, 0.0);
    let names = &job.names;
    fs::create_dir_all(&names.dir)
        .with_context(|| format!("Could not create {}", names.dir.display()))?;
    let target = names.m4b();
    let partial = target.with_extension("m4b.part");
    let copied = faststart::copy(
        &encoded,
        &partial,
        |fraction| report(Stage::Copying, fraction),
        cancel,
    )
    .and_then(|()| {
        let copied = ffmpeg::probe(tools, &partial)?.duration;
        ensure!(
            (copied - duration).abs() < 0.01,
            "The copy in the library doesn't play back correctly"
        );
        Ok(())
    });
    if let Err(error) = copied {
        fs::remove_file(&partial).ok();
        return Err(error);
    }
    fs::rename(&partial, &target)?;
    write_sidecars(job)?;
    Ok(target)
}

/// Converts a time on the probed timeline to a sample position on the exactly measured one.
fn exact_position(ms: u64, durations_ms: &[u64], lengths: &[u64], rate: u64) -> u64 {
    let mut probed_start = 0;
    let mut exact_start = 0;
    for (&duration, &length) in durations_ms.iter().zip(lengths) {
        if ms < probed_start + duration {
            return exact_start + ((ms - probed_start) * rate / 1000).min(length);
        }
        probed_start += duration;
        exact_start += length;
    }
    exact_start
}

/// Writes the same iTunes atoms the existing library uses, plus the chapters both as a Nero chapter
/// list and as a QuickTime chapter track, so every player finds them.
fn write_tags(path: &Path, job: &Job, chapters: &[Chapter]) -> Result<()> {
    let meta = &job.meta;
    let authors = meta.author_list();
    let narrators = meta.narrator_list().join(", ");
    let mut tag = Userdata::default();

    tag.set_title(&job.names.stem);
    tag.set_album(&meta.title);
    if let Some(author) = authors.first() {
        tag.set_album_artist(*author);
    }
    let artists: Vec<&str> = authors
        .iter()
        .copied()
        .chain(meta.narrator_list())
        .collect();
    tag.set_artist(artists.join(", "));
    set_if_present(&mut tag, &narrators, |tag, value| tag.set_composer(value));
    if let Some(year) = meta.year() {
        tag.set_year(year);
    }
    set_if_present(&mut tag, &meta.genre, |tag, value| {
        tag.set_custom_genre(value)
    });
    set_if_present(&mut tag, &meta.copyright, |tag, value| {
        tag.set_copyright(value)
    });
    set_if_present(&mut tag, &meta.description, |tag, value| {
        tag.set_description(value);
        tag.set_comment(value);
        tag.set_data(Fourcc(*b"ldes"), Data::Utf8(value.to_owned()));
    });
    if !meta.series.is_empty() {
        let part = meta.series_part.trim();
        tag.set_grouping(format!("{}, Book #{part}", meta.series));
        tag.set_album_sort_order(format!("{} {part} - {}", meta.series, meta.title));
        tag.set_movement(&meta.series);
        if let Ok(index) = part.parse::<u16>() {
            tag.set_movement_index(index);
        }
    }
    tag.set_media_type(MediaType::AudioBook);
    tag.set_gapless_playback();
    tag.set_track_number(1);
    if let Some(cover) = &job.cover {
        let bytes = cover.bytes.to_vec();
        tag.set_artwork(if cover.is_png() {
            Img::png(bytes)
        } else {
            Img::jpeg(bytes)
        });
    }

    let release_url = meta
        .region
        .filter(|_| !meta.asin.is_empty())
        .map(|r| r.product_url(&meta.asin));
    let freeform = [
        ("ASIN", meta.asin.clone()),
        ("SERIES", meta.series.clone()),
        ("SERIES-PART", meta.series_part.clone()),
        ("SUBTITLE", meta.subtitle.clone()),
        ("RELEASETIME", meta.release_date.clone()),
        ("PUBLISHER", meta.publisher.clone()),
        ("LANGUAGE", meta.language.clone()),
        ("FORMAT", meta.format_type.clone()),
        ("EXPLICIT", if meta.explicit { "1" } else { "0" }.to_owned()),
        ("WWWAUDIOFILE", release_url.unwrap_or_default()),
    ];
    for (name, value) in freeform {
        if !value.trim().is_empty() {
            tag.set_data(
                FreeformIdent::new_static("com.apple.iTunes", name),
                Data::Utf8(value),
            );
        }
    }

    let chapters: Vec<mp4ameta::Chapter> = chapters
        .iter()
        .map(|chapter| mp4ameta::Chapter {
            start: Duration::from_millis(chapter.start_ms),
            title: chapter.title.clone(),
        })
        .collect();
    *tag.chapter_list_mut() = chapters.clone();
    *tag.chapter_track_mut() = chapters;

    tag.write_with_path(path, &WriteConfig::DEFAULT)
        .context("Could not write the m4b tags")
}

fn set_if_present(tag: &mut Userdata, value: &str, set: impl FnOnce(&mut Userdata, &str)) {
    let value = value.trim();
    if !value.is_empty() {
        set(tag, value);
    }
}

/// The cover, desc.txt and reader.txt files that sit next to every book in the library.
fn write_sidecars(job: &Job) -> Result<()> {
    let names = &job.names;
    if let Some(cover) = &job.cover {
        fs::write(names.cover(cover.extension()), &cover.bytes)?;
    }
    let description = job.meta.description.trim();
    if !description.is_empty() {
        fs::write(names.description(), description)?;
    }
    let narrators = job.meta.narrator_list().join(", ");
    if !narrators.is_empty() {
        fs::write(names.reader(), narrators)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_aac_bitrate_from_source() {
        assert_eq!(matching_aac_bitrate(128, 1), 96);
        assert_eq!(matching_aac_bitrate(64, 1), 48);
        assert_eq!(matching_aac_bitrate(192, 2), 160);
        assert_eq!(matching_aac_bitrate(320, 2), 192);
        assert_eq!(matching_aac_bitrate(320, 1), 128);
        assert_eq!(matching_aac_bitrate(32, 1), 32);
    }

    #[test]
    fn maps_chapter_times_onto_measured_lengths() {
        // The second file is really 1 ms (44 samples) longer than probed.
        let durations = [1000, 2000, 3000];
        let lengths = [44_100, 88_244, 132_300];
        assert_eq!(exact_position(0, &durations, &lengths, 44_100), 0);
        assert_eq!(exact_position(1000, &durations, &lengths, 44_100), 44_100);
        assert_eq!(exact_position(1500, &durations, &lengths, 44_100), 66_150);
        assert_eq!(exact_position(3000, &durations, &lengths, 44_100), 132_344);
        assert_eq!(exact_position(9000, &durations, &lengths, 44_100), 264_644);
    }
}
