use std::path::{Path, PathBuf};

use crate::book::BookMeta;

/// Where a book goes in the library, following the existing layout:
/// `Author/[Series/]Year - Title[ Series Part]/Title (Year)[ Series Part].m4b`
#[derive(Clone, Debug, PartialEq)]
pub struct OutputNames {
    pub dir: PathBuf,
    pub stem: String,
}

impl OutputNames {
    pub fn new(root: &Path, meta: &BookMeta) -> Self {
        let author = meta
            .author_list()
            .first()
            .map(|author| sanitize(author))
            .filter(|author| !author.is_empty())
            .unwrap_or_else(|| "Unknown Author".to_owned());
        let title = Some(sanitize(&meta.title))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "Unknown Title".to_owned());
        let series = sanitize(&meta.series);
        let suffix = series_suffix(&title, &series, &sanitize(&meta.series_part));
        let year = meta.year();

        let folder = join_words([
            year.map(|y| format!("{y} -")),
            Some(title.clone()),
            suffix.clone(),
        ]);
        let stem = join_words([Some(title), year.map(|y| format!("({y})")), suffix]);
        let mut dir = root.join(author);
        if !series.is_empty() {
            dir.push(series);
        }
        dir.push(folder);
        Self { dir, stem }
    }

    pub fn m4b(&self) -> PathBuf {
        self.dir.join(format!("{}.m4b", self.stem))
    }

    pub fn cover(&self, extension: &str) -> PathBuf {
        self.dir.join(format!("{} - cover.{extension}", self.stem))
    }

    pub fn description(&self) -> PathBuf {
        self.dir.join("desc.txt")
    }

    pub fn reader(&self) -> PathBuf {
        self.dir.join("reader.txt")
    }
}

/// "Series Part", unless the title already ends with it (e.g. "King of Thorns Broken Empire 2").
fn series_suffix(title: &str, series: &str, part: &str) -> Option<String> {
    if series.is_empty() {
        return None;
    }
    let suffix = format!("{series} {part}").trim().to_owned();
    (!letters_and_digits(title).ends_with(&letters_and_digits(&suffix))).then_some(suffix)
}

fn letters_and_digits(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn join_words<const N: usize>(words: [Option<String>; N]) -> String {
    words
        .into_iter()
        .flatten()
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Makes a name safe for a Windows path component.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter_map(|c| match c {
            '/' | '\\' | '|' => Some(' '),
            '<' | '>' | ':' | '"' | '?' | '*' => None,
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.trim_end_matches(['.', ' ']).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(title: &str, authors: &str, series: &str, part: &str, date: &str) -> OutputNames {
        let meta = BookMeta {
            title: title.into(),
            authors: authors.into(),
            series: series.into(),
            series_part: part.into(),
            release_date: date.into(),
            ..BookMeta::default()
        };
        OutputNames::new(Path::new(r"\\nas\Audiobooks"), &meta)
    }

    #[test]
    fn book_in_a_series() {
        let out = names(
            "Pawn of Prophecy",
            "David Eddings",
            "The Belgariad",
            "1",
            "2007-05-01",
        );
        assert_eq!(
            out.m4b(),
            Path::new(
                r"\\nas\Audiobooks\David Eddings\The Belgariad\2007 - Pawn of Prophecy The Belgariad 1\Pawn of Prophecy (2007) The Belgariad 1.m4b"
            )
        );
    }

    #[test]
    fn standalone_book() {
        let out = names("Project Hail Mary", "Andy Weir", "", "", "2021");
        assert_eq!(
            out.dir,
            Path::new(r"\\nas\Audiobooks\Andy Weir\2021 - Project Hail Mary")
        );
        assert_eq!(out.stem, "Project Hail Mary (2021)");
    }

    #[test]
    fn uses_first_author_only() {
        let out = names("Abundance", "Ezra Klein, Derek Thompson", "", "", "2025");
        assert_eq!(
            out.dir,
            Path::new(r"\\nas\Audiobooks\Ezra Klein\2025 - Abundance")
        );
    }

    #[test]
    fn series_without_number_has_no_trailing_space() {
        let out = names(
            "Polgara the Sorceress",
            "Leigh Eddings",
            "Belgariad",
            "",
            "2010",
        );
        assert_eq!(out.stem, "Polgara the Sorceress (2010) Belgariad");
    }

    #[test]
    fn does_not_repeat_series_already_in_title() {
        let out = names(
            "King of Thorns: Broken Empire 2",
            "Mark Lawrence",
            "Broken Empire",
            "2",
            "2012",
        );
        assert_eq!(out.stem, "King of Thorns Broken Empire 2 (2012)");
    }

    #[test]
    fn strips_characters_windows_forbids() {
        assert_eq!(
            sanitize("What If?: Serious Answers"),
            "What If Serious Answers"
        );
        assert_eq!(sanitize("AC/DC..."), "AC DC");
    }
}
