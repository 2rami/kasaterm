//! 웹 카드 등록 — `mediaopen --block <주소>`. 쪽을 한 번 받아 제목과 대표 그림(og:image)을 붙인다.
//!
//! 쪽 스크린샷은 뜨지 않는다 — 웹뷰로 그리려면 사람 화면에 웹 칸을 하나 띄워야 해서다. 대표 그림이 없는 쪽은 그림
//! 없는 한 줄 카드로 선다. 쪽을 못 받아도(꺼진 개발 서버 등) 주소만으로 카드는 선다.

use std::time::Duration;

const MAX_HTML: usize = 2 * 1024 * 1024;
const MAX_IMAGE: usize = 8 * 1024 * 1024;
const MAX_TITLE_CHARS: usize = 120;

pub(crate) struct Page {
    pub title: String,
    /// 대표 그림 PNG(긴 변 1600 이하)와 그 크기.
    pub poster: Option<(Vec<u8>, u32, u32)>,
}

pub(crate) fn fetch(url: &str) -> Page {
    let base = reqwest::Url::parse(url).ok();
    let host = base.as_ref().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| url.to_owned());
    let html = get(url, MAX_HTML).ok().map(|b| String::from_utf8_lossy(&b).into_owned());
    let meta = html.as_deref().map(read_meta).unwrap_or_default();
    let poster = meta
        .image
        .and_then(|src| base.as_ref()?.join(&src).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .and_then(|u| get(u.as_str(), MAX_IMAGE).ok())
        .and_then(|bytes| poster(&bytes).ok());
    let title = meta.title.map(|t| clean_title(&t)).filter(|t| !t.is_empty()).unwrap_or(host);
    Page { title, poster }
}

fn get(url: &str, max: usize) -> anyhow::Result<Vec<u8>> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent("kasaterm")
            .build()?;
        let mut resp = client.get(url).send().await?.error_for_status()?;
        let mut out = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            anyhow::ensure!(out.len() + chunk.len() <= max, "response too large");
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    })
}

fn poster(bytes: &[u8]) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(10000);
    limits.max_image_height = Some(10000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.thumbnail(1600, 1600);
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png)?;
    let png = png.into_inner();
    anyhow::ensure!(png.len() <= kasa_socket::inline_media::MAX_POSTER_BYTES, "poster too large");
    Ok((png, image.width(), image.height()))
}

#[derive(Default, Debug, PartialEq)]
struct Meta {
    title: Option<String>,
    image: Option<String>,
}

/// 머리말만 본다 — `og:title`·`og:image`(없으면 `twitter:image`)·`<title>`. 브라우저처럼 해석하지 않고 태그 글자만 읽는다.
fn read_meta(html: &str) -> Meta {
    let head = &html[..html.len().min(512 * 1024)];
    let lower = head.to_ascii_lowercase();
    let mut meta = Meta::default();
    let mut twitter_image = None;
    let mut at = 0;
    while let Some(start) = lower[at..].find("<meta").map(|i| at + i) {
        let end = lower[start..].find('>').map_or(lower.len(), |i| start + i);
        let attrs = attributes(&head[start + 5..end]);
        let key = attrs.iter().find(|(k, _)| k == "property" || k == "name").map(|(_, v)| v.to_ascii_lowercase());
        let content = attrs.iter().find(|(k, _)| k == "content").map(|(_, v)| v.clone());
        match (key.as_deref(), content) {
            (Some("og:title"), Some(v)) if meta.title.is_none() => meta.title = Some(v),
            (Some("og:image" | "og:image:url"), Some(v)) if meta.image.is_none() => meta.image = Some(v),
            (Some("twitter:image"), Some(v)) => twitter_image = twitter_image.or(Some(v)),
            _ => {}
        }
        at = end;
    }
    if meta.title.is_none() {
        if let (Some(s), Some(e)) = (lower.find("<title"), lower.find("</title>")) {
            if let Some(open) = lower[s..].find('>').map(|i| s + i + 1).filter(|open| *open <= e) {
                meta.title = Some(entities(&head[open..e]));
            }
        }
    }
    meta.image = meta.image.or(twitter_image);
    meta
}

fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = tag;
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].trim().rsplit(|c: char| c.is_whitespace()).next().unwrap_or("").to_ascii_lowercase();
        let after = rest[eq + 1..].trim_start();
        let (value, next) = match after.chars().next() {
            Some(q @ ('"' | '\'')) => match after[1..].find(q) {
                Some(close) => (&after[1..1 + close], &after[close + 2..]),
                None => (&after[1..], ""),
            },
            _ => {
                let stop = after.find(char::is_whitespace).unwrap_or(after.len());
                (&after[..stop], &after[stop..])
            }
        };
        out.push((key, entities(value)));
        rest = next;
    }
    out
}

fn entities(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// 카드 이름이 곧 등록 파일 이름이라 경로 글자를 걷고 줄인다.
fn clean_title(title: &str) -> String {
    let joined = title.split_whitespace().collect::<Vec<_>>().join(" ");
    joined
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if matches!(c, '/' | '\\' | ':') { '·' } else { c })
        .take(MAX_TITLE_CHARS)
        .collect::<String>()
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_string()
}

/// 주소를 담은 `.url` 을 임시 폴더에 쓰고 등록한다. 등록부가 복사해 두므로 임시 파일은 바로 지운다.
pub(crate) fn register(url: &str) -> anyhow::Result<kasa_socket::inline_media::Media> {
    anyhow::ensure!(kasa_socket::inline_media::valid_web_url(url), "http(s) 주소만 웹 카드로 만들 수 있습니다");
    let page = fetch(url);
    let dir = std::env::temp_dir().join(format!("kasaterm-web-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.url", page.title));
    let result = std::fs::write(&file, format!("{url}\n")).map_err(anyhow::Error::from).and_then(|()| {
        let probe = match page.poster {
            Some((png, w, h)) => kasa_socket::inline_media::Probe { width: Some(w), height: Some(h), poster: Some(png), ..Default::default() },
            None => kasa_socket::inline_media::Probe::default(),
        };
        Ok(kasa_socket::inline_media::register(&file, probe)?)
    });
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_tags_give_title_and_image_in_any_quoting() {
        let html = r#"<html><head><title> Old &amp; plain </title>
            <meta name=twitter:image content=/tw.png>
            <META property="og:title" content="랜딩 &quot;시안&quot;" />
            <meta content='https://cdn.example/og.png' property='og:image'></head>"#;
        assert_eq!(
            read_meta(html),
            Meta { title: Some("랜딩 \"시안\"".into()), image: Some("https://cdn.example/og.png".into()) }
        );
        let bare = read_meta("<title>Only Title</title><meta name='twitter:image' content='/t.png'>");
        assert_eq!(bare, Meta { title: Some("Only Title".into()), image: Some("/t.png".into()) });
        assert_eq!(read_meta("no head at all"), Meta::default());
    }

    #[test]
    fn titles_become_safe_file_names() {
        assert_eq!(clean_title("  a/b\\c:d \n e.. "), "a·b·c·d e");
        assert_eq!(clean_title(&"가".repeat(300)).chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn an_unreachable_page_still_makes_a_card_named_after_its_host() {
        let page = fetch("http://127.0.0.1:9/never");
        assert_eq!(page.title, "127.0.0.1");
        assert!(page.poster.is_none());
    }
}
