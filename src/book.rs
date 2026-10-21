use std::path::Path;

use crate::{
    audible::{Product, Region},
    source::SourceBook,
    text::{collapse_whitespace, decode_entities, split_names},
};

/// The metadata written to the m4b, editable in the UI. Name lists are comma separated.
#[derive(Clone, Debug, Default)]
pub struct BookMeta {
    pub title: String,
    pub subtitle: String,
    pub authors: String,
    pub narrators: String,
    pub series: String,
    pub series_part: String,
    /// `YYYY-MM-DD` from Audible, or just a year from local sources.
    pub release_date: String,
    pub publisher: String,
    pub genre: String,
    pub language: String,
    pub copyright: String,
    pub format_type: String,
    pub description: String,
    pub asin: String,
    pub region: Option<Region>,
    pub explicit: bool,
}

impl BookMeta {
    /// Fills in what the NFO and the audio tags know, NFO first.
    pub fn from_source(source: &SourceBook) -> Self {
        let nfo = source.nfo.clone().unwrap_or_default();
        let tag = |key: &str| source.tag(key).map(str::to_owned);
        let not_generic = |genre: &String| !genre.eq_ignore_ascii_case("audiobook");
        let (asin, region) = source.asin.clone().unzip();
        Self {
            title: nfo
                .title
                .or_else(|| tag("album"))
                .unwrap_or_else(|| folder_title(&source.dir)),
            authors: nfo
                .author
                .or_else(|| tag("album_artist"))
                .or_else(|| tag("artist"))
                .unwrap_or_default(),
            narrators: nfo
                .narrator
                .or_else(|| tag("narratedby"))
                .or_else(|| tag("composer"))
                .unwrap_or_default(),
            series: nfo.series.or_else(|| tag("series")).unwrap_or_default(),
            series_part: nfo
                .series_part
                .or_else(|| tag("series-part"))
                .unwrap_or_default(),
            release_date: nfo
                .year
                .or_else(|| tag("date").map(|date| date.chars().take(4).collect()))
                .unwrap_or_default(),
            publisher: nfo
                .publisher
                .or_else(|| tag("publisher"))
                .unwrap_or_default(),
            genre: nfo
                .genre
                .filter(not_generic)
                .or_else(|| tag("genre").filter(not_generic))
                .unwrap_or_default(),
            description: nfo
                .description
                .or_else(|| tag("description"))
                .or_else(|| tag("comment"))
                .unwrap_or_default(),
            asin: asin.unwrap_or_default(),
            region: region.flatten(),
            ..Self::default()
        }
    }

    /// Overwrites every field Audible has a value for.
    pub fn apply_audible(&mut self, product: &Product) {
        fn set(field: &mut String, value: &str) {
            let value = collapse_whitespace(&decode_entities(value));
            if !value.is_empty() {
                *field = value;
            }
        }
        // Audible lists translators and editors as authors, e.g. "Ken Liu - translator".
        let without_roles = |names: &[String]| {
            names
                .iter()
                .filter(|name| !name.contains(" - "))
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        set(&mut self.title, &product.title);
        self.subtitle = decode_entities(&product.subtitle);
        set(&mut self.authors, &without_roles(&product.authors));
        set(&mut self.narrators, &product.narrators.join(", "));
        if let Some(series) = product.series.first() {
            self.series = decode_entities(&series.name);
            self.series_part = series.position.clone();
        }
        set(&mut self.release_date, &product.release_date);
        set(&mut self.publisher, &product.publisher);
        let genres: Vec<String> = product
            .genres
            .iter()
            .map(|ladder| ladder.join(" / "))
            .collect();
        set(&mut self.genre, &genres.join(";"));
        set(&mut self.language, &product.language);
        set(&mut self.copyright, &product.copyright);
        set(&mut self.format_type, &product.format_type);
        if !product.description.is_empty() {
            self.description = product.description.clone();
        }
        self.asin = product.asin.clone();
        self.region = Some(product.region);
        self.explicit = product.explicit;
    }

    pub fn year(&self) -> Option<&str> {
        let year = self.release_date.trim().get(..4)?;
        year.chars().all(|c| c.is_ascii_digit()).then_some(year)
    }

    pub fn author_list(&self) -> Vec<&str> {
        split_names(&self.authors)
    }

    pub fn narrator_list(&self) -> Vec<&str> {
        split_names(&self.narrators)
    }
}

/// "The Faith of Beasts [B0FBCFCBG1] (2026)" -> "The Faith of Beasts"
fn folder_title(dir: &Path) -> String {
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    let mut title = String::new();
    let mut depth = 0;
    for c in name.chars() {
        match c {
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => depth = (depth - 1).max(0),
            _ if depth == 0 => title.push(if c == '_' { ' ' } else { c }),
            _ => {}
        }
    }
    collapse_whitespace(&title)
}
