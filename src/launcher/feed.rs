//! Small RSS / Atom reading shared by Radio's podcast list and Infoscope's News.
//!
//! No XML crate: feeds are scanned for the few elements we need. [`xml_text`], [`tag`] and
//! [`item_blocks`] are exactly what Radio's podcast reader used before (moved here unchanged);
//! [`parse_news`] adds titles, links, dates and summaries for RSS 2.0, RSS 1.0 (RDF) and Atom.

use serde::{Deserialize, Serialize};

/// CDATA unwrapped, the five XML entities decoded (Radio's original behaviour).
pub fn xml_text(s: &str) -> String {
    let s = s.trim().strip_prefix("<![CDATA[").and_then(|s| s.strip_suffix("]]>")).unwrap_or(s.trim());
    s.replace("&quot;", "\"").replace("&apos;", "'")
        .replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// The text of the first `<name>...</name>` (no attributes) in `s`.
pub fn tag(s: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&close)? + start;
    Some(xml_text(&s[start..end]))
}

/// The bodies of `<item ...>...</item>` blocks, in order.
pub fn item_blocks(xml: &str) -> Vec<&str> {
    blocks(xml, "item")
}

fn blocks<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}");
    let close = format!("</{name}>");
    let mut out = Vec::new();
    let mut remaining = xml;
    while let Some(at) = remaining.find(&open) {
        remaining = &remaining[at + open.len()..];
        let Some(open_end) = remaining.find('>') else { break };
        remaining = &remaining[open_end + 1..];
        let Some(end) = remaining.find(&close) else { break };
        out.push(&remaining[..end]);
        remaining = &remaining[end + close.len()..];
    }
    out
}

// ------------------------------------------------------------------ news

/// One headline. `date` is seconds since the epoch (UTC) when the feed gave a readable one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsItem {
    pub title: String,
    pub link: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub date: Option<i64>,
    #[serde(default)]
    pub summary: String,
    /// The fullest text the feed carries (`content:encoded`, Atom `content`, else the
    /// description) as HTML, for reading without the article page. Empty when only the summary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
}

/// A parsed feed: its own title and its items (newest first as the feed lists them).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedFeed {
    pub title: String,
    pub items: Vec<NewsItem>,
}

pub const MAX_ITEMS: usize = 40;
const SUMMARY_MAX: usize = 400;
const CONTENT_MAX: usize = 12_000;

pub fn http_url(s: &str) -> bool {
    (s.starts_with("https://") || s.starts_with("http://"))
        && !s.chars().any(char::is_control)
        && !s.contains(' ')
        && s.len() <= 4096
}

/// Decode the XML entities plus numeric ones (`&#8217;`, `&#x2019;`) and the common HTML ones.
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // Byte search: a `;` within 12 bytes (a str slice there could cut a CJK character).
        let end = rest.as_bytes()[..rest.len().min(12)].iter().position(|&b| b == b';');
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                "hellip" => Some('…'),
                "mdash" => Some('—'),
                "ndash" => Some('–'),
                "rsquo" | "lsquo" => Some('\''),
                "rdquo" | "ldquo" => Some('"'),
                _ => {
                    let n = if let Some(h) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                        u32::from_str_radix(h, 16).ok()
                    } else {
                        name.strip_prefix('#').and_then(|d| d.parse().ok())
                    };
                    n.and_then(char::from_u32).filter(|c| !c.is_control() || *c == '\n')
                }
            };
            ch.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Remove `<...>` markup and fold every run of white space into one space.
pub fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Plain text of an element body: CDATA unwrapped, entities decoded (twice when the feed escaped
/// its HTML), markup removed, white space folded.
fn plain(body: &str) -> String {
    let b = body.trim();
    let b = b.strip_prefix("<![CDATA[").and_then(|s| s.strip_suffix("]]>")).unwrap_or(b);
    let once = decode_entities(b);
    let text = strip_markup(&once);
    // Escaped HTML (`&lt;p&gt;`) only becomes markup after the first decode.
    if once.contains('<') { decode_entities(&text) } else { text }
}

/// An element body as HTML: CDATA unwrapped, or the escaped markup decoded once.
fn markup(body: &str) -> String {
    let b = body.trim();
    match b.strip_prefix("<![CDATA[").and_then(|s| s.strip_suffix("]]>")) {
        Some(raw) => raw.to_string(),
        None => decode_entities(b),
    }
}

/// Where `<name` starts as a whole element name (followed by `>`, `/` or white space), from `from`.
fn find_open(s: &str, name: &str, from: usize) -> Option<usize> {
    let open = format!("<{name}");
    let mut at = from;
    while let Some(i) = s[at..].find(&open) {
        let i = at + i;
        match s.as_bytes().get(i + open.len()) {
            Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => return Some(i),
            _ => at = i + open.len(),
        }
    }
    None
}

/// The attributes text and the body of the first `<name ...>body</name>` (or `<name .../>`).
fn element<'a>(s: &'a str, name: &str) -> Option<(&'a str, &'a str)> {
    let start = find_open(s, name, 0)?;
    let after = start + 1 + name.len();
    let gt = s[after..].find('>')? + after;
    if s.as_bytes()[gt - 1] == b'/' {
        return Some((&s[after..gt - 1], ""));
    }
    let close = format!("</{name}>");
    let end = s[gt + 1..].find(&close)? + gt + 1;
    Some((&s[after..gt], &s[gt + 1..end]))
}

/// Every `<name .../>` or `<name ...>` opening tag's attribute text (for Atom's links).
fn openings<'a>(s: &'a str, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = find_open(s, name, at) {
        let after = i + 1 + name.len();
        let Some(gt) = s[after..].find('>') else { break };
        out.push(s[after..after + gt].trim_end_matches('/'));
        at = after + gt;
    }
    out
}

fn attr(attrs: &str, name: &str) -> Option<String> {
    let key = format!("{name}=");
    for (i, _) in attrs.match_indices(&key) {
        if i > 0 && !attrs.as_bytes()[i - 1].is_ascii_whitespace() {
            continue;
        }
        let value = &attrs[i + key.len()..];
        let quote = value.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let rest = &value[1..];
        let finish = rest.find(quote)?;
        return Some(decode_entities(&rest[..finish]));
    }
    None
}

fn text_of(s: &str, names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| element(s, n).map(|(_, b)| plain(b)).filter(|t| !t.is_empty()))
}

/// RFC 2822 (RSS `pubDate`) or RFC 3339 (Atom, `dc:date`) to seconds since the epoch.
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    chrono::DateTime::parse_from_rfc2822(s)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(s))
        .ok()
        .map(|d| d.timestamp())
        .or_else(|| {
            // Some feeds use a bare date or a zone name chrono does not know ("JST").
            let t = s.rsplit_once(' ').map(|(a, z)| if z.chars().all(|c| c.is_ascii_uppercase()) && z.len() == 3 { a } else { s }).unwrap_or(s);
            chrono::DateTime::parse_from_rfc2822(&format!("{t} +0000")).ok().map(|d| d.timestamp())
                .or_else(|| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0)).map(|d| d.and_utc().timestamp()))
        })
}

fn clip(s: String, max: usize) -> String {
    if s.chars().count() <= max { s } else { format!("{}…", s.chars().take(max).collect::<String>().trim_end()) }
}

/// The link of an Atom entry: `rel="alternate"` (or no rel) first, then any http link.
fn atom_link(entry: &str) -> Option<String> {
    let links = openings(entry, "link");
    links.iter().find(|a| matches!(attr(a, "rel").as_deref(), None | Some("alternate")))
        .or(links.first())
        .and_then(|a| attr(a, "href"))
}

/// Read an RSS 2.0, RSS 1.0 or Atom feed. Items without a title or an http link are dropped.
pub fn parse_news(xml: &str) -> ParsedFeed {
    let atom = find_open(xml, "feed", 0).is_some() && find_open(xml, "entry", 0).is_some();
    let (name, item_name) = if atom { ("feed", "entry") } else { ("channel", "item") };
    // The feed's own title: before the first item (RSS 1.0 closes <channel> before its items).
    let head_end = find_open(xml, item_name, 0).unwrap_or(xml.len());
    let head = &xml[..head_end];
    let head = find_open(head, name, 0).map(|i| &head[i..]).unwrap_or(head);
    let title = text_of(head, &["title"]).unwrap_or_default();
    let mut items = Vec::new();
    for b in blocks(xml, item_name) {
        let Some(t) = text_of(b, &["title"]) else { continue };
        let link = if atom {
            atom_link(b)
        } else {
            text_of(b, &["link"]).or_else(|| element(b, "guid").filter(|(a, _)| !a.contains("isPermaLink=\"false\"")).map(|(_, g)| plain(g)))
        };
        let Some(link) = link.map(|l| l.trim().to_string()).filter(|l| http_url(l)) else { continue };
        let date = text_of(b, &["pubDate", "dc:date", "updated", "published"]).and_then(|d| parse_date(&d));
        let summary = text_of(b, &["description", "summary", "content:encoded", "content"]).unwrap_or_default();
        let content = ["content:encoded", "content", "description", "summary"].iter()
            .find_map(|n| element(b, n).map(|(_, body)| markup(body)).filter(|h| !strip_markup(h).is_empty()))
            .map(|h| clip(h, CONTENT_MAX))
            .filter(|h| strip_markup(h).chars().count() > summary.chars().count().min(SUMMARY_MAX))
            .unwrap_or_default();
        items.push(NewsItem { title: clip(t, 200), link, source: title.clone(), date, summary: clip(summary, SUMMARY_MAX), content });
        if items.len() >= MAX_ITEMS {
            break;
        }
    }
    ParsedFeed { title, items }
}

/// All items of several feeds, newest first (undated ones after the dated, in feed order),
/// each link once, at most `max`.
pub fn merge(feeds: &[Vec<NewsItem>], max: usize) -> Vec<NewsItem> {
    let mut all: Vec<(usize, NewsItem)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for items in feeds {
        for it in items {
            if seen.insert(it.link.clone()) {
                all.push((all.len(), it.clone()));
            }
        }
    }
    all.sort_by(|(ia, a), (ib, b)| b.date.unwrap_or(i64::MIN).cmp(&a.date.unwrap_or(i64::MIN)).then(ia.cmp(ib)));
    all.into_iter().map(|(_, it)| it).take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel>
      <title>Harbour Daily</title><link>https://news.example/</link>
      <item><title><![CDATA[Tides &amp; boats]]></title><link>https://news.example/a?x=1&amp;y=2</link>
        <pubDate>Wed, 18 Nov 2026 09:30:00 +0900</pubDate>
        <description>&lt;p&gt;Ships came &lt;b&gt;in&lt;/b&gt; early.&lt;/p&gt;</description></item>
      <item><title>It&#8217;s calm</title><guid>https://news.example/b</guid><pubDate>Thu, 19 Nov 2026 01:00:00 GMT</pubDate></item>
      <item><title>No link</title></item>
      <item><title>Bad link</title><link>javascript:alert(1)</link></item>
    </channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?><feed xmlns="http://www.w3.org/2005/Atom">
      <title type="text">Lighthouse Log</title><link href="https://log.example/"/>
      <entry><title type="html">Lamp &lt;i&gt;lit&lt;/i&gt;</title>
        <link rel="replies" href="https://log.example/1#c"/><link rel="alternate" href="https://log.example/1"/>
        <updated>2026-11-19T08:00:00Z</updated><summary>Bright night.</summary></entry>
      <entry><title>Fog</title><link href='https://log.example/2'/><published>2026-11-17T08:00:00+09:00</published></entry>
    </feed>"#;

    const RDF: &str = r#"<rdf:RDF><channel rdf:about="https://r.example/"><title>Old Style</title></channel>
      <item rdf:about="https://r.example/1"><title>First</title><link>https://r.example/1</link><dc:date>2026-11-01T00:00:00+09:00</dc:date></item>
    </rdf:RDF>"#;

    #[test]
    fn rss2_titles_links_dates_and_summaries() {
        let f = parse_news(RSS);
        assert_eq!(f.title, "Harbour Daily");
        assert_eq!(f.items.len(), 2);
        let a = &f.items[0];
        assert_eq!(a.title, "Tides & boats");
        assert_eq!(a.link, "https://news.example/a?x=1&y=2");
        assert_eq!(a.source, "Harbour Daily");
        assert_eq!(a.date, Some(1_794_961_800)); // 2026-11-18 00:30 UTC
        assert_eq!(a.summary, "Ships came in early.");
        assert_eq!(f.items[1].title, "It\u{2019}s calm");
        assert_eq!(f.items[1].link, "https://news.example/b");
    }

    #[test]
    fn atom_prefers_the_alternate_link() {
        let f = parse_news(ATOM);
        assert_eq!(f.title, "Lighthouse Log");
        assert_eq!(f.items.len(), 2);
        assert_eq!(f.items[0].title, "Lamp lit");
        assert_eq!(f.items[0].link, "https://log.example/1");
        assert_eq!(f.items[0].summary, "Bright night.");
        assert_eq!(f.items[1].link, "https://log.example/2");
        assert!(f.items[0].date > f.items[1].date);
    }

    #[test]
    fn rss1_rdf_items_outside_the_channel() {
        let f = parse_news(RDF);
        assert_eq!(f.title, "Old Style");
        assert_eq!(f.items.len(), 1);
        assert!(f.items[0].date.is_some());
    }

    #[test]
    fn garbage_gives_nothing_and_never_panics() {
        for s in ["", "<rss", "<item><title>x", "<feed><entry><link href=\"", "&#xFFFFFFFF; &#0; &", "<item>\u{3042}</item>"] {
            let _ = parse_news(s);
            let _ = decode_entities(s);
        }
        assert!(parse_news("<html><body>Not a feed</body></html>").items.is_empty());
    }

    #[test]
    fn cjk_after_an_ampersand_never_panics() {
        // Every offset of a 3-byte character relative to the 12-byte window.
        for pad in 0..4 {
            let s = format!("&{}日本語のニュース &amp;記事 & テスト", "a".repeat(pad));
            let _ = decode_entities(&s);
        }
        assert_eq!(decode_entities("&amp;日本語"), "&日本語");
        assert_eq!(decode_entities("A&B 東京都の天気"), "A&B 東京都の天気");
        assert_eq!(decode_entities("&#26085;本"), "日本");
    }

    #[test]
    fn dates_in_the_usual_forms() {
        assert_eq!(parse_date("Thu, 19 Nov 2026 01:00:00 GMT"), Some(1_795_050_000));
        assert_eq!(parse_date("2026-11-19T01:00:00Z"), Some(1_795_050_000));
        assert_eq!(parse_date("Thu, 19 Nov 2026 01:00:00 JST"), Some(1_795_050_000));
        assert_eq!(parse_date("2026-11-19"), Some(1_795_046_400));
        assert_eq!(parse_date("yesterday"), None);
    }

    #[test]
    fn merge_sorts_newest_first_and_drops_repeats() {
        let it = |t: &str, d: Option<i64>| NewsItem { title: t.into(), link: format!("https://x/{t}"), date: d, ..Default::default() };
        let a = vec![it("a", Some(10)), it("b", None)];
        let b = vec![it("c", Some(30)), it("a", Some(10)), it("d", Some(20))];
        let m: Vec<String> = merge(&[a, b], 10).into_iter().map(|i| i.title).collect();
        assert_eq!(m, ["c", "d", "a", "b"]);
    }

    #[test]
    fn radio_helpers_keep_their_behaviour() {
        assert_eq!(xml_text(" <![CDATA[a &amp; b]]> "), "a & b");
        assert_eq!(tag("<x><title>T</title></x>", "title").as_deref(), Some("T"));
        assert_eq!(item_blocks("<item a='1'>one</item><item>two</item>"), ["one", "two"]);
    }

    #[test]
    fn content_keeps_the_fullest_text_as_html() {
        let xml = r#"<rss><channel><title>T</title>
          <item><title>A</title><link>https://x/a</link><description>Short.</description>
            <content:encoded><![CDATA[<p>First paragraph.</p><p>Second &amp; last.</p>]]></content:encoded></item>
          <item><title>B</title><link>https://x/b</link><description>Only this.</description></item>
        </channel></rss>"#;
        let f = parse_news(xml);
        assert_eq!(f.items[0].summary, "Short.");
        assert_eq!(f.items[0].content, "<p>First paragraph.</p><p>Second &amp; last.</p>");
        assert_eq!(f.items[1].content, "", "nothing beyond the summary");
        // Escaped HTML in a description is decoded once into markup.
        let g = parse_news(RSS);
        assert_eq!(g.items[0].content, "");
    }
}
