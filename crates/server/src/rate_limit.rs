//! Rate limit for MCP tool calls, per token.
//!
//! Each token has a token bucket. The bucket holds at most `burst` calls and refills at
//! `per_minute / 60` calls per second. A client can send a short burst, and over a long time
//! it gets `per_minute` calls each minute.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use tokio::time::Instant;

/// Time between two removals of idle buckets.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("rate limit exceeded, retry in {retry_after_secs} s")]
pub struct RateLimited {
    pub retry_after_secs: u64,
}

pub struct RateLimiter {
    /// `None` when the limit is off.
    rate: Option<Rate>,
    state: Mutex<Buckets>,
}

#[derive(Clone, Copy)]
struct Rate {
    burst: f64,
    per_sec: f64,
}

struct Buckets {
    by_token: HashMap<i64, Bucket>,
    last_prune: Instant,
}

struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl RateLimiter {
    /// `per_minute = 0` turns the limit off. The burst is the calls of 10 seconds, at least 1.
    pub fn new(per_minute: u32) -> Self {
        let rate = (per_minute > 0).then(|| Rate {
            burst: per_minute.div_ceil(6) as f64,
            per_sec: f64::from(per_minute) / 60.0,
        });
        Self {
            rate,
            state: Mutex::new(Buckets {
                by_token: HashMap::new(),
                last_prune: Instant::now(),
            }),
        }
    }

    /// Takes one call from the bucket of the token, or tells how long to wait.
    pub fn check(&self, token_id: i64) -> Result<(), RateLimited> {
        let Some(rate) = self.rate else {
            return Ok(());
        };
        let now = Instant::now();
        // A panic while the lock is held cannot leave the map in a bad state, so a poisoned
        // lock is safe to use.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if now.duration_since(state.last_prune) >= PRUNE_INTERVAL {
            state.last_prune = now;
            // A full bucket acts the same as a new bucket, so removing it changes nothing.
            state
                .by_token
                .retain(|_, bucket| bucket.refilled(rate, now) < rate.burst);
        }
        let bucket = state.by_token.entry(token_id).or_insert(Bucket {
            tokens: rate.burst,
            updated: now,
        });
        bucket.tokens = bucket.refilled(rate, now);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            return Ok(());
        }
        let wait = (1.0 - bucket.tokens) / rate.per_sec;
        Err(RateLimited {
            retry_after_secs: (wait.ceil() as u64).max(1),
        })
    }

    /// The number of buckets in memory. Tests use it to check that idle buckets go away.
    pub fn bucket_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .by_token
            .len()
    }
}

impl Bucket {
    fn refilled(&self, rate: Rate, now: Instant) -> f64 {
        let elapsed = now.duration_since(self.updated).as_secs_f64();
        (self.tokens + elapsed * rate.per_sec).min(rate.burst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(limiter: &RateLimiter, token_id: i64, count: usize) -> usize {
        (0..count)
            .filter(|_| limiter.check(token_id).is_ok())
            .count()
    }

    #[tokio::test(start_paused = true)]
    async fn calls_over_the_burst_are_refused() {
        let limiter = RateLimiter::new(60);
        assert_eq!(take(&limiter, 1, 10), 10);
        assert_eq!(
            limiter.check(1),
            Err(RateLimited {
                retry_after_secs: 1
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn bucket_refills_over_time() {
        let limiter = RateLimiter::new(60);
        take(&limiter, 1, 10);
        assert!(limiter.check(1).is_err());

        tokio::time::advance(Duration::from_secs(3)).await;
        assert_eq!(take(&limiter, 1, 10), 3);

        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(take(&limiter, 1, 20), 10);
    }

    #[tokio::test(start_paused = true)]
    async fn steady_rate_is_one_call_per_refill_step() {
        let limiter = RateLimiter::new(60);
        take(&limiter, 1, 10);
        for _ in 0..60 {
            tokio::time::advance(Duration::from_secs(1)).await;
            assert_eq!(take(&limiter, 1, 10), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retry_time_matches_the_refill() {
        let limiter = RateLimiter::new(30);
        take(&limiter, 1, 5);
        assert_eq!(
            limiter.check(1),
            Err(RateLimited {
                retry_after_secs: 2
            })
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            limiter.check(1),
            Err(RateLimited {
                retry_after_secs: 1
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn each_token_has_its_own_bucket() {
        let limiter = RateLimiter::new(60);
        assert_eq!(take(&limiter, 1, 20), 10);
        assert_eq!(take(&limiter, 2, 20), 10);
    }

    #[tokio::test(start_paused = true)]
    async fn zero_turns_the_limit_off() {
        let limiter = RateLimiter::new(0);
        assert_eq!(take(&limiter, 1, 10_000), 10_000);
        assert_eq!(limiter.bucket_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn idle_buckets_are_removed() {
        let limiter = RateLimiter::new(60);
        take(&limiter, 1, 5);
        take(&limiter, 2, 5);
        assert_eq!(limiter.bucket_count(), 2);

        tokio::time::advance(PRUNE_INTERVAL).await;
        // The check of token 3 starts the removal. Tokens 1 and 2 have full buckets again.
        take(&limiter, 3, 1);
        assert_eq!(limiter.bucket_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn busy_buckets_are_kept() {
        let limiter = RateLimiter::new(6);
        take(&limiter, 1, 1);
        tokio::time::advance(PRUNE_INTERVAL - Duration::from_secs(1)).await;
        take(&limiter, 1, 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        take(&limiter, 2, 1);
        assert_eq!(limiter.bucket_count(), 2);
    }
}
