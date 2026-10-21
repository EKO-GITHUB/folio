use std::{collections::HashMap, sync::LazyLock, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::text::html_to_text;

static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .user_agent(concat!("folio/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("HTTP client")
});

const SEARCH_GROUPS: &str = "contributors,media,product_attrs,series";
const DETAIL_GROUPS: &str = "contributors,media,product_attrs,product_desc,product_details,product_extended_attrs,series,category_ladders";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Region {
    Us,
    Uk,
    De,
    Fr,
    Ca,
    Au,
    It,
    Es,
    In,
    Jp,
}

impl Region {
    pub const ALL: [Region; 10] = [
        Region::Us,
        Region::Uk,
        Region::De,
        Region::Fr,
        Region::Ca,
        Region::Au,
        Region::It,
        Region::Es,
        Region::In,
        Region::Jp,
    ];

    /// The region code Audnexus expects.
    pub fn code(self) -> &'static str {
        match self {
            Region::Us => "us",
            Region::Uk => "uk",
            Region::De => "de",
            Region::Fr => "fr",
            Region::Ca => "ca",
            Region::Au => "au",
            Region::It => "it",
            Region::Es => "es",
            Region::In => "in",
            Region::Jp => "jp",
        }
    }

    /// The top level domain of the marketplace, e.g. `co.uk` for audible.co.uk.
    pub fn domain(self) -> &'static str {
        match self {
            Region::Us => "com",
            Region::Uk => "co.uk",
            Region::De => "de",
            Region::Fr => "fr",
            Region::Ca => "ca",
            Region::Au => "com.au",
            Region::It => "it",
            Region::Es => "es",
            Region::In => "in",
            Region::Jp => "co.jp",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Region::Us => "US",
            Region::Uk => "UK",
            Region::De => "DE",
            Region::Fr => "FR",
            Region::Ca => "CA",
            Region::Au => "AU",
            Region::It => "IT",
            Region::Es => "ES",
            Region::In => "IN",
            Region::Jp => "JP",
        }
    }

    pub fn from_domain(domain: &str) -> Option<Region> {
        let domain = domain.to_ascii_lowercase();
        Region::ALL
            .into_iter()
            .find(|region| region.domain() == domain)
    }

    pub fn product_url(self, asin: &str) -> String {
        format!("https://www.audible.{}/pd/{asin}", self.domain())
    }
}

#[derive(Clone, Debug)]
pub struct SeriesRef {
    pub name: String,
    pub position: String,
}

/// An Audible catalog product. Search results only fill the fields covered by `SEARCH_GROUPS`.
#[derive(Clone, Debug)]
pub struct Product {
    pub asin: String,
    pub region: Region,
    pub title: String,
    pub subtitle: String,
    pub authors: Vec<String>,
    pub narrators: Vec<String>,
    pub series: Vec<SeriesRef>,
    pub publisher: String,
    pub release_date: String,
    pub language: String,
    pub format_type: String,
    pub explicit: bool,
    pub runtime_min: Option<u32>,
    /// Category ladders, e.g. `["Science Fiction & Fantasy", "Fantasy", "Epic Fantasy"]`.
    pub genres: Vec<Vec<String>>,
    pub description: String,
    pub copyright: String,
    pub cover_url: Option<String>,
    pub thumb_url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AudibleChapter {
    pub start_ms: u64,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct AudibleChapters {
    pub runtime_ms: u64,
    pub chapters: Vec<AudibleChapter>,
}

/// Searches each region in turn and returns all hits, in region order.
pub fn search(regions: &[Region], title: &str, author: &str) -> Result<Vec<Product>> {
    let mut hits = Vec::new();
    let mut last_error = None;
    for &region in regions {
        match search_region(region, title, author) {
            Ok(mut found) => hits.append(&mut found),
            Err(error) => last_error = Some(error),
        }
    }
    match last_error {
        Some(error) if hits.is_empty() => Err(error),
        _ => Ok(hits),
    }
}

fn search_region(region: Region, title: &str, author: &str) -> Result<Vec<Product>> {
    let url = format!(
        "https://api.audible.{}/1.0/catalog/products",
        region.domain()
    );
    let common = [
        ("num_results", "10"),
        ("products_sort_by", "Relevance"),
        ("response_groups", SEARCH_GROUPS),
        ("image_sizes", "500"),
    ];
    let mut query = common.to_vec();
    query.push(("title", title));
    if !author.is_empty() {
        query.push(("author", author));
    }
    let mut response: ProductsResponse = get_json(&url, &query)?;
    if response.products.is_empty() {
        let keywords = format!("{title} {author}");
        let mut query = common.to_vec();
        query.push(("keywords", keywords.trim()));
        response = get_json(&url, &query)?;
    }
    Ok(response
        .products
        .into_iter()
        .filter(|product| product.title.is_some())
        .map(|product| product.into_product(region))
        .collect())
}

pub fn product(asin: &str, region: Region) -> Result<Product> {
    let url = format!(
        "https://api.audible.{}/1.0/catalog/products/{asin}",
        region.domain()
    );
    let query = [
        ("response_groups", DETAIL_GROUPS),
        ("image_sizes", "500,2400"),
    ];
    let response: ProductResponse = get_json(&url, &query)?;
    if response.product.title.is_none() {
        bail!("{asin} is not available on audible.{}", region.domain());
    }
    Ok(response.product.into_product(region))
}

/// Chapter timings aren't in the public catalog API, so they come from Audnexus.
pub fn chapters(asin: &str, region: Region) -> Result<AudibleChapters> {
    let url = format!("https://api.audnex.us/books/{asin}/chapters");
    let info: ApiChapterInfo = get_json(&url, &[("region", region.code())])?;
    let runtime_ms = info
        .runtime_length_ms
        .or_else(|| {
            info.chapters
                .last()
                .map(|last| last.start_offset_ms + last.length_ms)
        })
        .context("Audnexus returned no runtime")?;
    let chapters = info
        .chapters
        .into_iter()
        .map(|chapter| AudibleChapter {
            start_ms: chapter.start_offset_ms,
            title: chapter.title,
        })
        .collect();
    Ok(AudibleChapters {
        runtime_ms,
        chapters,
    })
}

pub fn download(url: &str) -> Result<Vec<u8>> {
    let response = CLIENT.get(url).send()?.error_for_status()?;
    Ok(response.bytes()?.to_vec())
}

fn get_json<T: DeserializeOwned>(url: &str, query: &[(&str, &str)]) -> Result<T> {
    let response = CLIENT
        .get(url)
        .query(query)
        .send()
        .with_context(|| format!("Could not reach {url}"))?
        .error_for_status()?;
    Ok(response.json()?)
}

#[derive(Deserialize)]
struct ProductsResponse {
    #[serde(default)]
    products: Vec<ApiProduct>,
}

#[derive(Deserialize)]
struct ProductResponse {
    product: ApiProduct,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiProduct {
    asin: String,
    title: Option<String>,
    subtitle: Option<String>,
    authors: Option<Vec<ApiName>>,
    narrators: Option<Vec<ApiName>>,
    series: Option<Vec<ApiSeries>>,
    publisher_name: Option<String>,
    release_date: Option<String>,
    issue_date: Option<String>,
    language: Option<String>,
    format_type: Option<String>,
    is_adult_product: Option<bool>,
    runtime_length_min: Option<u32>,
    product_images: Option<HashMap<String, String>>,
    publisher_summary: Option<String>,
    merchandising_summary: Option<String>,
    copyright: Option<String>,
    category_ladders: Option<Vec<ApiLadder>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiName {
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiSeries {
    title: String,
    sequence: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiLadder {
    root: String,
    ladder: Vec<ApiName>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiChapterInfo {
    #[serde(default)]
    chapters: Vec<ApiChapter>,
    runtime_length_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiChapter {
    start_offset_ms: u64,
    length_ms: u64,
    title: String,
}

impl ApiProduct {
    fn into_product(self, region: Region) -> Product {
        let names = |list: Option<Vec<ApiName>>| {
            list.unwrap_or_default()
                .into_iter()
                .map(|n| n.name.trim().to_owned())
                .collect()
        };
        let mut images: Vec<(u32, String)> = self
            .product_images
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(size, url)| Some((size.parse().ok()?, url)))
            .collect();
        images.sort_by_key(|(size, _)| *size);
        let summary = self
            .publisher_summary
            .or(self.merchandising_summary)
            .unwrap_or_default();
        Product {
            asin: self.asin,
            region,
            title: self.title.unwrap_or_default().trim().to_owned(),
            subtitle: self.subtitle.unwrap_or_default().trim().to_owned(),
            authors: names(self.authors),
            narrators: names(self.narrators),
            series: self
                .series
                .unwrap_or_default()
                .into_iter()
                .map(|s| SeriesRef {
                    name: s.title.trim().to_owned(),
                    position: s.sequence.unwrap_or_default().trim().to_owned(),
                })
                .collect(),
            publisher: self.publisher_name.unwrap_or_default(),
            release_date: self.release_date.or(self.issue_date).unwrap_or_default(),
            language: self.language.unwrap_or_default(),
            format_type: self.format_type.unwrap_or_default(),
            explicit: self.is_adult_product.unwrap_or(false),
            runtime_min: self.runtime_length_min,
            genres: self
                .category_ladders
                .unwrap_or_default()
                .into_iter()
                .filter(|ladder| ladder.root == "Genres")
                .map(|ladder| ladder.ladder.into_iter().map(|n| n.name).collect())
                .collect(),
            description: html_to_text(&summary),
            copyright: self.copyright.unwrap_or_default(),
            thumb_url: images.first().map(|(_, url)| url.clone()),
            cover_url: images.pop().map(|(_, url)| url),
        }
    }
}
