use std::time::Duration;

use serde_json::{json, Value};
use tracing::warn;

const USER_AGENT: &str = "OurCivicAtlas/0.1 civic research (+https://ourcivicatlas.org)";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(12);
const PER_PROVIDER_LIMIT: usize = 8;

#[derive(Debug, Clone)]
struct LiveEvidence {
    provider: &'static str,
    source_type: &'static str,
    title: String,
    snippet: String,
    url: String,
    confidence: f64,
}

impl LiveEvidence {
    fn to_search_record(&self, index: usize) -> Value {
        json!({
            "resultId": format!("live:{}:{}", self.provider, stable_id(&self.url, index)),
            "kind": self.source_type,
            "label": self.title,
            "snippet": self.snippet,
            "relevanceScore": self.confidence,
            "confidence": self.confidence,
            "source": self.source_type,
            "url": self.url,
            "closesGapId": "",
        })
    }
}

pub async fn discover_live_evidence(query: &str, max_results: usize) -> Vec<Value> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let Ok(client) = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
    else {
        return Vec::new();
    };

    let provider_limit = PER_PROVIDER_LIMIT.min(max_results.max(1));
    let (general_web, internet_archive, library_of_congress) = tokio::join!(
        duckduckgo_html(&client, trimmed, provider_limit),
        internet_archive(&client, trimmed, provider_limit),
        library_of_congress(&client, trimmed, provider_limit),
    );

    interleave_sources(
        vec![general_web, internet_archive, library_of_congress],
        max_results,
    )
    .into_iter()
    .enumerate()
    .map(|(index, evidence)| evidence.to_search_record(index))
    .collect()
}

async fn duckduckgo_html(client: &reqwest::Client, query: &str, limit: usize) -> Vec<LiveEvidence> {
    let response = match client
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", query)])
        .header(reqwest::header::ACCEPT, "text/html")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            warn!(%error, "duckduckgo live evidence request failed");
            return Vec::new();
        }
    };

    if !response.status().is_success() {
        warn!(
            status = response.status().as_u16(),
            "duckduckgo live evidence returned non-success status",
        );
        return Vec::new();
    }

    match response.text().await {
        Ok(html) => parse_duckduckgo_html(&html, limit),
        Err(error) => {
            warn!(%error, "duckduckgo live evidence body read failed");
            Vec::new()
        }
    }
}

async fn internet_archive(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Vec<LiveEvidence> {
    let rows = limit.to_string();
    let archive_query = format!("({query}) AND mediatype:(texts OR image OR web OR software)");
    let params = [
        ("q", archive_query.as_str()),
        ("fl[]", "identifier"),
        ("fl[]", "title"),
        ("fl[]", "description"),
        ("fl[]", "mediatype"),
        ("fl[]", "date"),
        ("fl[]", "creator"),
        ("rows", rows.as_str()),
        ("page", "1"),
        ("output", "json"),
        ("sort[]", "downloads desc"),
    ];
    let response = match client
        .get("https://archive.org/advancedsearch.php")
        .query(&params)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            warn!(%error, "internet archive live evidence request failed");
            return Vec::new();
        }
    };

    if !response.status().is_success() {
        warn!(
            status = response.status().as_u16(),
            "internet archive live evidence returned non-success status",
        );
        return Vec::new();
    }

    let payload = match response.json::<Value>().await {
        Ok(payload) => payload,
        Err(error) => {
            warn!(%error, "internet archive live evidence JSON parse failed");
            return Vec::new();
        }
    };

    let docs = payload
        .get("response")
        .and_then(|response| response.get("docs"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    docs.into_iter()
        .filter_map(|doc| {
            let identifier = stringify_value(doc.get("identifier")).trim().to_string();
            if identifier.is_empty() {
                return None;
            }
            let title =
                non_empty(stringify_value(doc.get("title"))).unwrap_or_else(|| identifier.clone());
            let mediatype = stringify_value(doc.get("mediatype"));
            Some(LiveEvidence {
                provider: "internet_archive",
                source_type: archive_source_type(&mediatype),
                title,
                snippet: stringify_value(doc.get("description")),
                url: format!("https://archive.org/details/{identifier}"),
                confidence: if mediatype == "texts" { 0.78 } else { 0.62 },
            })
        })
        .take(limit)
        .collect()
}

async fn library_of_congress(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Vec<LiveEvidence> {
    let count = limit.to_string();
    let params = [("q", query), ("fo", "json"), ("c", count.as_str())];
    let response = match client
        .get("https://www.loc.gov/search/")
        .query(&params)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            warn!(%error, "library of congress live evidence request failed");
            return Vec::new();
        }
    };

    if !response.status().is_success() {
        warn!(
            status = response.status().as_u16(),
            "library of congress live evidence returned non-success status",
        );
        return Vec::new();
    }

    let payload = match response.json::<Value>().await {
        Ok(payload) => payload,
        Err(error) => {
            warn!(%error, "library of congress live evidence JSON parse failed");
            return Vec::new();
        }
    };

    payload
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|result| {
            let title = non_empty(stringify_value(result.get("title")))?;
            let url = pick_loc_url(result)?;
            let partof = stringify_value(result.get("partof"));
            Some(LiveEvidence {
                provider: "library_of_congress",
                source_type: loc_source_type(&partof),
                title,
                snippet: stringify_value(result.get("description")),
                url,
                confidence: 0.82,
            })
        })
        .take(limit)
        .collect()
}

fn interleave_sources(sources: Vec<Vec<LiveEvidence>>, max_results: usize) -> Vec<LiveEvidence> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let max_len = sources.iter().map(Vec::len).max().unwrap_or(0);

    for index in 0..max_len {
        for source in &sources {
            let Some(candidate) = source.get(index) else {
                continue;
            };
            if !seen.insert(candidate.url.clone()) {
                continue;
            }
            out.push(candidate.clone());
            if out.len() >= max_results {
                return out;
            }
        }
    }
    out
}

fn parse_duckduckgo_html(html: &str, limit: usize) -> Vec<LiveEvidence> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cursor = 0;

    while out.len() < limit {
        let Some(class_offset) = html[cursor..].find("result__a") else {
            break;
        };
        let class_index = cursor + class_offset;
        let anchor_start = html[..class_index].rfind("<a").unwrap_or(class_index);
        let Some(anchor_end_offset) = html[class_index..].find("</a>") else {
            break;
        };
        let anchor_end = class_index + anchor_end_offset + "</a>".len();
        let anchor = &html[anchor_start..anchor_end];
        cursor = anchor_end;

        let Some(raw_href) = extract_attr(anchor, "href") else {
            continue;
        };
        let url = unwrap_duckduckgo_href(&raw_href);
        if !url.starts_with("http://") && !url.starts_with("https://") {
            continue;
        }
        if !seen.insert(url.clone()) {
            continue;
        }

        let title = strip_html(anchor);
        if title.is_empty() {
            continue;
        }
        let snippet = parse_next_duckduckgo_snippet(html, cursor);
        out.push(LiveEvidence {
            provider: "duckduckgo_html",
            source_type: "general_web",
            title,
            snippet,
            url,
            confidence: 0.56,
        });
    }
    out
}

fn parse_next_duckduckgo_snippet(html: &str, from: usize) -> String {
    let window = &html[from..html.len().min(from + 2400)];
    let Some(snippet_class) = window.find("result__snippet") else {
        return String::new();
    };
    let Some(anchor_start_rel) = window[..snippet_class].rfind("<a") else {
        return String::new();
    };
    let Some(anchor_end_rel) = window[snippet_class..].find("</a>") else {
        return String::new();
    };
    let anchor_end = snippet_class + anchor_end_rel + "</a>".len();
    strip_html(&window[anchor_start_rel..anchor_end])
}

fn extract_attr(html: &str, attr: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let needle = format!("{attr}={quote}");
        let Some(start) = html.find(&needle).map(|index| index + needle.len()) else {
            continue;
        };
        let rest = &html[start..];
        let Some(end) = rest.find(quote) else {
            continue;
        };
        return Some(rest[..end].to_string());
    }
    None
}

fn unwrap_duckduckgo_href(href: &str) -> String {
    let normalized = if href.starts_with("//") {
        format!("https:{href}")
    } else if href.starts_with('/') {
        format!("https://duckduckgo.com{href}")
    } else {
        href.to_string()
    };

    let Ok(url) = reqwest::Url::parse(&normalized) else {
        return normalized;
    };
    if url
        .domain()
        .is_some_and(|domain| domain.ends_with("duckduckgo.com"))
    {
        for (key, value) in url.query_pairs() {
            if key == "uddg" || key == "u" {
                return value.into_owned();
            }
        }
    }
    normalized
}

fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    decode_entities(out.trim()).trim().to_string()
}

fn decode_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

fn pick_loc_url(result: &Value) -> Option<String> {
    let primary = result.get("id").and_then(Value::as_str).unwrap_or("");
    if primary.starts_with("http") {
        return Some(primary.to_string());
    }
    result
        .get("url")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|value| {
            let url = value.as_str()?;
            url.starts_with("http").then(|| url.to_string())
        })
}

fn stringify_value(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| stringify_value(Some(value)))
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Object(map)) => map
            .values()
            .map(|value| stringify_value(Some(value)))
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Some(value) => value.to_string(),
    }
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn loc_source_type(partof: &str) -> &'static str {
    let haystack = partof.to_lowercase();
    if haystack.contains("sanborn") {
        "sanborn_map"
    } else if haystack.contains("historic american buildings")
        || haystack.contains("historic american engineering")
        || haystack.contains("historic american landscapes")
    {
        "habs_haer"
    } else if haystack.contains("photograph") {
        "historic_photo"
    } else {
        "library_of_congress"
    }
}

fn archive_source_type(mediatype: &str) -> &'static str {
    match mediatype {
        "texts" => "archive_text",
        "image" => "archive_image",
        "web" => "archive_web",
        "software" => "archive_software",
        _ => "archive_item",
    }
}

fn stable_id(url: &str, fallback: usize) -> String {
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for byte in url.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    if hash == 0 {
        fallback.to_string()
    } else {
        format!("{hash:016x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duckduckgo_results_and_unwraps_target_url() {
        let html = r#"
          <a class="result__a" href="//duckduckgo.com/l/?kh=-1&uddg=https%3A%2F%2Fexample.com%2Fhistory">
            Flint &amp; history
          </a>
          <a class="result__snippet">A useful <b>source</b>.</a>
        "#;

        let results = parse_duckduckgo_html(html, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/history");
        assert_eq!(results[0].title, "Flint & history");
        assert_eq!(results[0].snippet, "A useful source.");
        assert_eq!(results[0].source_type, "general_web");
    }

    #[test]
    fn interleaves_general_web_before_loc_dominates() {
        let web = LiveEvidence {
            provider: "duckduckgo_html",
            source_type: "general_web",
            title: "Open web".to_string(),
            snippet: String::new(),
            url: "https://example.com".to_string(),
            confidence: 0.5,
        };
        let loc = LiveEvidence {
            provider: "library_of_congress",
            source_type: "library_of_congress",
            title: "LoC".to_string(),
            snippet: String::new(),
            url: "https://www.loc.gov/item/1".to_string(),
            confidence: 0.8,
        };

        let results = interleave_sources(vec![vec![web.clone()], vec![], vec![loc]], 2);

        assert_eq!(results[0].url, web.url);
        assert_eq!(results.len(), 2);
    }
}
