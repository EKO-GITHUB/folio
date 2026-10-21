use crate::audible::AudibleChapters;

/// How far the local runtime may be off from Audible's before its chapter timings are distrusted.
const RUNTIME_TOLERANCE_MS: u64 = 5_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Chapter {
    pub start_ms: u64,
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChapterOrigin {
    /// Audible's own chapters, because the runtime matches the files.
    AudibleTimings,
    /// One chapter per file, named after Audible's chapters because the counts match.
    AudibleTitles,
    PerFile,
}

impl ChapterOrigin {
    pub fn describe(self) -> &'static str {
        match self {
            ChapterOrigin::AudibleTimings => "Audible chapters (runtime matches)",
            ChapterOrigin::AudibleTitles => "one per file, Audible titles (chapter count matches)",
            ChapterOrigin::PerFile => "one per file",
        }
    }
}

pub fn per_file(durations_ms: &[u64]) -> Vec<Chapter> {
    file_starts(durations_ms)
        .enumerate()
        .map(|(i, start_ms)| Chapter {
            start_ms,
            title: format!("Chapter {}", i + 1),
        })
        .collect()
}

/// Picks Audible timings, then Audible titles on file boundaries, then "Chapter N" per file.
pub fn best(
    durations_ms: &[u64],
    audible: Option<&AudibleChapters>,
) -> (Vec<Chapter>, ChapterOrigin) {
    let total_ms: u64 = durations_ms.iter().sum();
    if let Some(audible) = audible.filter(|audible| !audible.chapters.is_empty()) {
        if audible.runtime_ms.abs_diff(total_ms) <= RUNTIME_TOLERANCE_MS {
            let chapters = audible
                .chapters
                .iter()
                .filter(|chapter| chapter.start_ms < total_ms)
                .map(|chapter| Chapter {
                    start_ms: chapter.start_ms,
                    title: chapter.title.clone(),
                })
                .collect();
            return (chapters, ChapterOrigin::AudibleTimings);
        }
        if audible.chapters.len() == durations_ms.len() {
            let chapters = file_starts(durations_ms)
                .zip(&audible.chapters)
                .map(|(start_ms, chapter)| Chapter {
                    start_ms,
                    title: chapter.title.clone(),
                })
                .collect();
            return (chapters, ChapterOrigin::AudibleTitles);
        }
    }
    (per_file(durations_ms), ChapterOrigin::PerFile)
}

fn file_starts(durations_ms: &[u64]) -> impl Iterator<Item = u64> + '_ {
    durations_ms.iter().scan(0, |start, duration| {
        let this = *start;
        *start += duration;
        Some(this)
    })
}

pub fn format_ms(ms: u64) -> String {
    let secs = ms / 1000;
    format!(
        "{}:{:02}:{:02}.{:03}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        ms % 1000
    )
}

/// Parses `h:mm:ss.mmm`, `mm:ss` or plain seconds.
pub fn parse_ms(text: &str) -> Option<u64> {
    let mut secs = 0.0;
    for part in text.trim().split(':') {
        let value: f64 = part.trim().parse().ok()?;
        if !value.is_finite() || value < 0.0 {
            return None;
        }
        secs = secs * 60.0 + value;
    }
    Some((secs * 1000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audible::AudibleChapter;

    fn audible(runtime_ms: u64, starts: &[u64]) -> AudibleChapters {
        AudibleChapters {
            runtime_ms,
            chapters: starts
                .iter()
                .enumerate()
                .map(|(i, &start_ms)| AudibleChapter {
                    start_ms,
                    title: format!("A{i}"),
                })
                .collect(),
        }
    }

    #[test]
    fn uses_audible_timings_when_runtime_matches() {
        let (chapters, origin) = best(&[1000, 2000], Some(&audible(3001, &[0, 500, 2500])));
        assert_eq!(origin, ChapterOrigin::AudibleTimings);
        assert_eq!(
            chapters.iter().map(|c| c.start_ms).collect::<Vec<_>>(),
            [0, 500, 2500]
        );
    }

    #[test]
    fn uses_audible_titles_when_counts_match() {
        let (chapters, origin) = best(&[60_000, 60_000], Some(&audible(600_000, &[0, 1])));
        assert_eq!(origin, ChapterOrigin::AudibleTitles);
        assert_eq!(
            chapters[1],
            Chapter {
                start_ms: 60_000,
                title: "A1".into()
            }
        );
    }

    #[test]
    fn falls_back_to_one_per_file() {
        let (chapters, origin) = best(&[60_000, 60_000], Some(&audible(600_000, &[0, 1, 2])));
        assert_eq!(origin, ChapterOrigin::PerFile);
        assert_eq!(
            chapters[1],
            Chapter {
                start_ms: 60_000,
                title: "Chapter 2".into()
            }
        );
    }

    #[test]
    fn round_trips_timestamps() {
        assert_eq!(format_ms(3_723_456), "1:02:03.456");
        assert_eq!(parse_ms("1:02:03.456"), Some(3_723_456));
        assert_eq!(parse_ms("02:03"), Some(123_000));
        assert_eq!(parse_ms("x"), None);
    }
}
