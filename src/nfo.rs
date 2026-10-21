use std::{fs, path::Path, sync::LazyLock};

use regex::Regex;

use crate::text::collapse_whitespace;

static YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(1[89]\d\d|20\d\d)\b").unwrap());
static DURATION_PART: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(\d+)\s*(h|m|s)").unwrap());

/// The fields of a release NFO (e.g. the "General Information" block written by inAudible).
#[derive(Debug, Default, Clone)]
pub struct Nfo {
    pub title: Option<String>,
    pub author: Option<String>,
    pub narrator: Option<String>,
    pub series: Option<String>,
    pub series_part: Option<String>,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub publisher: Option<String>,
    pub description: Option<String>,
    pub duration_secs: Option<f64>,
    pub chapters: Option<usize>,
    /// The raw text, kept so ASINs and Audible links can be searched for.
    pub raw: String,
}

pub fn read(path: &Path) -> anyhow::Result<Nfo> {
    Ok(parse(&decode(&fs::read(path)?)))
}

fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

pub fn parse(text: &str) -> Nfo {
    let lines: Vec<&str> = text.lines().collect();
    let description_start = lines
        .windows(2)
        .position(|pair| is_underline(pair[1]) && pair[0].to_lowercase().contains("description"));
    let header_lines = &lines[..description_start.unwrap_or(lines.len())];

    let mut nfo = Nfo {
        raw: text.to_owned(),
        ..Nfo::default()
    };
    let mut copyright_year = None;
    let mut audiobook_year = None;
    for line in header_lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = collapse_whitespace(value);
        if value.is_empty() {
            continue;
        }
        let slot = match key.trim().to_lowercase().as_str() {
            "title" | "book title" => &mut nfo.title,
            "author" | "authors" | "written by" => &mut nfo.author,
            "read by" | "narrator" | "narrators" | "narrated by" => &mut nfo.narrator,
            "series" | "series name" => &mut nfo.series,
            "series position" | "series number" | "series part" | "book number" => {
                &mut nfo.series_part
            }
            "genre" => &mut nfo.genre,
            "publisher" => &mut nfo.publisher,
            "audiobook copyright" | "release date" | "released" => &mut audiobook_year,
            "copyright" | "year" | "published" => &mut copyright_year,
            "duration" | "runtime" | "length" => {
                nfo.duration_secs = parse_duration(&value);
                continue;
            }
            "chapters" => {
                nfo.chapters = value.parse().ok();
                continue;
            }
            _ => continue,
        };
        slot.get_or_insert(value);
    }
    nfo.year = audiobook_year
        .or(copyright_year)
        .and_then(|text| YEAR.find(&text).map(|m| m.as_str().to_owned()));
    nfo.description = description_start
        .map(|start| section_text(&lines[start + 2..]))
        .filter(|text| !text.is_empty());
    nfo
}

fn is_underline(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 3 && line.chars().all(|c| c == '=' || c == '-')
}

/// Collects the text of a section, stopping at the next underlined heading.
fn section_text(lines: &[&str]) -> String {
    let end = lines
        .windows(2)
        .position(|pair| is_underline(pair[1]))
        .unwrap_or(lines.len());
    let paragraphs: Vec<String> = lines[..end]
        .split(|line| line.trim().is_empty())
        .map(|paragraph| collapse_whitespace(&paragraph.join(" ")))
        .filter(|paragraph| !paragraph.is_empty())
        .collect();
    paragraphs.join("\n\n")
}

/// Accepts "14 hours, 28 minutes, 52 seconds", "14h 28m 52s" and "14:28:52".
fn parse_duration(text: &str) -> Option<f64> {
    if text.contains(':') {
        return text
            .split(':')
            .map(|part| part.trim().parse::<f64>().ok())
            .try_fold(0.0, |total, part| Some(total * 60.0 + part?));
    }
    let secs: f64 = DURATION_PART
        .captures_iter(text)
        .map(|caps| {
            let value: f64 = caps[1].parse().unwrap_or(0.0);
            match caps[2].to_ascii_lowercase().as_str() {
                "h" => value * 3600.0,
                "m" => value * 60.0,
                _ => value,
            }
        })
        .sum();
    (secs > 0.0).then_some(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INAUDIBLE: &str = "General Information
===================
 Title:                  The Faith of Beasts
 Author:                 James S. A. Corey
 Read By:                Jefferson Mays
 Copyright:              2025
 Audiobook Copyright:    2026
 Genre:                  Audiobook
 Publisher:              Recorded Books
 Duration:               14 hours, 28 minutes, 52 seconds
 Chapters:               44


Media Information
=================
 Source Format:          Audible AAX
 Encoded Codec:          LAME MP3

Book Description
================
First line of the
description.

Second paragraph.
";

    #[test]
    fn parses_inaudible_nfo() {
        let nfo = parse(INAUDIBLE);
        assert_eq!(nfo.title.as_deref(), Some("The Faith of Beasts"));
        assert_eq!(nfo.author.as_deref(), Some("James S. A. Corey"));
        assert_eq!(nfo.narrator.as_deref(), Some("Jefferson Mays"));
        assert_eq!(nfo.year.as_deref(), Some("2026"));
        assert_eq!(nfo.publisher.as_deref(), Some("Recorded Books"));
        assert_eq!(nfo.duration_secs, Some(52132.0));
        assert_eq!(nfo.chapters, Some(44));
        assert_eq!(
            nfo.description.as_deref(),
            Some("First line of the description.\n\nSecond paragraph.")
        );
    }

    #[test]
    fn empty_description_is_none() {
        let nfo = parse("Title: X\n\nBook Description\n================\n\n");
        assert_eq!(nfo.description, None);
    }

    #[test]
    fn parses_clock_durations() {
        assert_eq!(parse_duration("1:02:03"), Some(3723.0));
        assert_eq!(parse_duration("2h 5m"), Some(7500.0));
    }
}
