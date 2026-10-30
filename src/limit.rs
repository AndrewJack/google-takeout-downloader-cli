//! A cap on the total download speed, shared by every part.

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// How far ahead of the rate callers may get before they have to wait, so small timing jitter
/// doesn't cost throughput.
const BURST: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
#[error("invalid rate {0:?}: use bytes per second with an optional K, M or G suffix, e.g. 5M")]
pub struct RateError(String);

/// Parses a rate in bytes per second: `500000`, `800K`, `5M`, `1.5G` (1024-based, any case,
/// optional trailing `B`/`iB`).
pub fn parse_rate(s: &str) -> Result<u64, RateError> {
    let err = || RateError(s.to_owned());
    let t = s.trim();
    let t = t
        .strip_suffix("iB")
        .or_else(|| t.strip_suffix("ib"))
        .or_else(|| t.strip_suffix(['B', 'b']))
        .unwrap_or(t);
    let (num, mult) = match t.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => {
            let mult: u64 = match c.to_ascii_lowercase() {
                'k' => 1 << 10,
                'm' => 1 << 20,
                'g' => 1 << 30,
                _ => return Err(err()),
            };
            (&t[..i], mult)
        }
        _ => (t, 1),
    };
    let n: f64 = num.trim().parse().map_err(|_| err())?;
    if !n.is_finite() || n < 0.0 {
        return Err(err());
    }
    Ok((n * mult as f64).round() as u64)
}

/// Spreads `rate` bytes per second across every caller of [`RateLimiter::consume`].
pub struct RateLimiter {
    secs_per_byte: f64,
    /// When the bytes handed out so far will have been "paid for" at the rate.
    next_free: Mutex<Option<Instant>>,
}

impl RateLimiter {
    /// `bytes_per_sec` must be above zero.
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            secs_per_byte: 1.0 / bytes_per_sec.max(1) as f64,
            next_free: Mutex::new(None),
        }
    }

    /// Accounts for `bytes` just received, sleeping as long as needed to stay under the rate.
    pub async fn consume(&self, bytes: usize) {
        let now = Instant::now();
        let wake = {
            let mut next = self.next_free.lock().unwrap_or_else(|e| e.into_inner());
            let start = next.map_or(now, |n| n.max(now));
            let end = start + Duration::from_secs_f64(bytes as f64 * self.secs_per_byte);
            *next = Some(end);
            end.checked_sub(BURST).unwrap_or(end)
        };
        if wake > now {
            tokio::time::sleep_until(wake).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rates() {
        assert_eq!(parse_rate("1000").unwrap(), 1000);
        assert_eq!(parse_rate("800K").unwrap(), 800 * 1024);
        assert_eq!(parse_rate("800k").unwrap(), 800 * 1024);
        assert_eq!(parse_rate("5M").unwrap(), 5 << 20);
        assert_eq!(parse_rate("5MB").unwrap(), 5 << 20);
        assert_eq!(parse_rate("5MiB").unwrap(), 5 << 20);
        assert_eq!(parse_rate("1.5m").unwrap(), 3 << 19);
        assert_eq!(parse_rate("2G").unwrap(), 2 << 30);
        assert_eq!(parse_rate(" 0 ").unwrap(), 0);
    }

    #[test]
    fn rejects_bad_rates() {
        for bad in ["", "M", "abc", "5X", "-1M", "1..5M", "inf", "NaN"] {
            assert!(parse_rate(bad).is_err(), "{bad:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn holds_callers_to_the_rate() {
        let limit = RateLimiter::new(1000);
        let started = Instant::now();
        // 3000 bytes from two callers at 1000 B/s: about 3 s, less the burst allowance.
        tokio::join!(
            async {
                for _ in 0..15 {
                    limit.consume(100).await;
                }
            },
            async {
                for _ in 0..15 {
                    limit.consume(100).await;
                }
            },
        );
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(2700), "{took:?}");
        assert!(took <= Duration::from_millis(3100), "{took:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn idle_time_is_not_banked_beyond_the_burst() {
        let limit = RateLimiter::new(1000);
        limit.consume(100).await;
        tokio::time::sleep(Duration::from_secs(10)).await;
        let started = Instant::now();
        limit.consume(1000).await;
        // A long idle spell only earns the 250 ms burst, not 10 s of credit.
        assert!(started.elapsed() >= Duration::from_millis(700));
    }
}
