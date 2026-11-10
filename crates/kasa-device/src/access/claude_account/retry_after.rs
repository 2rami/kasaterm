use std::time::{Duration, Instant, SystemTime};

use reqwest::header::{HeaderMap, DATE, RETRY_AFTER};

const FALLBACK: Duration = Duration::from_secs(15 * 60);

pub(super) fn deadline(headers: &HeaderMap, now: Instant, wall_now: SystemTime) -> Instant {
    let delay = headers.get(RETRY_AFTER).and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let value = value.trim();
            if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
                return value.parse::<u64>().ok().map(Duration::from_secs);
            }
            let until = httpdate::parse_http_date(value).ok()?;
            // A server Date keeps local clock skew from shortening its requested wait.
            let server_now = headers.get(DATE).and_then(|value| value.to_str().ok())
                .and_then(|value| httpdate::parse_http_date(value).ok()).unwrap_or(wall_now);
            Some(until.duration_since(server_now).unwrap_or_default())
        });
    // An unrepresentable header must not panic or turn throttling into an immediate retry.
    delay.and_then(|delay| now.checked_add(delay)).unwrap_or(now + FALLBACK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn headers(retry: &str, date: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, retry.parse().unwrap());
        if let Some(date) = date { headers.insert(DATE, date.parse().unwrap()); }
        headers
    }

    #[test]
    fn server_seconds_replace_the_fixed_wait_in_both_directions() {
        let now = Instant::now();
        for (value, seconds) in [("37", 37), (" 120 ", 120), ("0", 0), ("86400", 86_400)] {
            assert_eq!(deadline(&headers(value, None), now, SystemTime::UNIX_EPOCH), now + Duration::from_secs(seconds));
        }
    }

    #[test]
    fn http_dates_use_the_server_clock_when_available() {
        let now = Instant::now();
        let date = "Sun, 06 Nov 1994 08:49:07 GMT";
        for retry in ["Sun, 06 Nov 1994 08:49:37 GMT", "Sunday, 06-Nov-94 08:49:37 GMT", "Sun Nov  6 08:49:37 1994"] {
            assert_eq!(deadline(&headers(retry, Some(date)), now, SystemTime::UNIX_EPOCH), now + Duration::from_secs(30));
        }
    }

    #[test]
    fn dates_without_a_server_clock_use_the_receipt_time() {
        let now = Instant::now();
        let wall = httpdate::parse_http_date("Sun, 06 Nov 1994 08:49:07 GMT").unwrap();
        for date in [None, Some("invalid")] {
            assert_eq!(deadline(&headers("Sun, 06 Nov 1994 08:49:37 GMT", date), now, wall), now + Duration::from_secs(30));
        }
        assert_eq!(deadline(&headers("Sun, 06 Nov 1994 08:48:00 GMT", None), now, wall), now);
    }

    #[test]
    fn missing_invalid_or_unrepresentable_values_keep_the_existing_fallback() {
        let now = Instant::now();
        let expected = now + FALLBACK;
        assert_eq!(deadline(&HeaderMap::new(), now, SystemTime::UNIX_EPOCH), expected);
        for value in ["", "-1", "+1", "1.5", "NaN", "60 seconds", "not a date", "999999999999999999999999999999999999"] {
            assert_eq!(deadline(&headers(value, None), now, SystemTime::UNIX_EPOCH), expected, "{value}");
        }
        assert_eq!(deadline(&headers(&u64::MAX.to_string(), None), now, SystemTime::UNIX_EPOCH), expected);
        let mut bytes = HeaderMap::new();
        bytes.insert(RETRY_AFTER, HeaderValue::from_bytes(&[0xff]).unwrap());
        assert_eq!(deadline(&bytes, now, SystemTime::UNIX_EPOCH), expected);
    }
}
