use std::sync::LazyLock;

use regex::{Captures, Regex};

static BLOCK_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<br\s*/?>|</(p|li|div|h\d)\s*>").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]*>").unwrap());
static ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);").unwrap());

/// Turns Audible's HTML summaries into plain text with blank lines between paragraphs.
pub fn html_to_text(html: &str) -> String {
    let text = BLOCK_END.replace_all(html, "\n");
    let text = TAG.replace_all(&text, "");
    decode_entities(&text)
        .lines()
        .map(collapse_whitespace)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn decode_entities(text: &str) -> String {
    ENTITY
        .replace_all(text, |caps: &Captures| {
            let name = &caps[1];
            let decoded = match name.strip_prefix('#') {
                Some(number) => {
                    let code = match number.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => number.parse().ok(),
                    };
                    code.and_then(char::from_u32).map(String::from)
                }
                None => named_entity(name).map(String::from),
            };
            decoded.unwrap_or_else(|| caps[0].to_owned())
        })
        .into_owned()
}

fn named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        _ => return None,
    })
}

pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Splits a comma separated list of names, dropping empty entries.
pub fn split_names(list: &str) -> Vec<&str> {
    list.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect()
}

pub fn format_duration(secs: f64) -> String {
    let total = secs.round() as u64;
    format!(
        "{}h {:02}m {:02}s",
        total / 3600,
        total / 60 % 60,
        total % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_audible_html() {
        let html = "<p><b>Bold</b> intro&nbsp;text.</p><p>Second &mdash; part&#39;s <i>end</i></p>";
        assert_eq!(
            html_to_text(html),
            "Bold intro text.\n\nSecond — part's end"
        );
    }

    #[test]
    fn leaves_unknown_entities_alone() {
        assert_eq!(decode_entities("a &bogus; b &#x263A;"), "a &bogus; b ☺");
    }
}
