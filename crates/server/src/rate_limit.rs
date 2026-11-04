//! Rate limits for MCP tool calls.
//!
//! Each token has a token bucket, and each user has one more bucket that all tokens of all
//! servers of the user share. A bucket holds at most `burst` calls and refills at
//! `per_minute / 60` calls per second. A client can send a short burst, and over a long time
//! it gets `per_minute` calls each minute. A call must pass both buckets.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;
use tokio::time::Instant;
use ts_rs::TS;

use crate::{auth::AuthUser, state::AppState};

/// Time between two removals of idle buckets.
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RateLimited {
    #[error("rate limit exceeded for this token, retry in {retry_after_secs} s")]
    Token { retry_after_secs: u64 },
    #[error("rate limit exceeded for this account, retry in {retry_after_secs} s")]
    Account { retry_after_secs: u64 },
}

impl RateLimited {
    pub fn retry_after_secs(&self) -> u64 {
        match self {
            Self::Token { retry_after_secs } | Self::Account { retry_after_secs } => {
                *retry_after_secs
            }
        }
    }

    pub fn scope(&self) -> &'static str {
        match self {
            Self::Token { .. } => "token",
            Self::Account { .. } => "account",
        }
    }
}

/// The limits as the dashboard shows them. The values of a limit are 0 when it is off.
#[derive(Debug, Clone, Copy, Serialize, TS)]
#[ts(export)]
pub struct Limits {
    pub rate_limit_per_minute: u32,
    pub rate_limit_burst: u32,
    pub user_rate_limit_per_minute: u32,
    pub user_rate_limit_burst: u32,
}

pub struct RateLimiter {
    limits: Limits,
    /// `None` when the limit is off.
    token_rate: Option<Rate>,
    user_rate: Option<Rate>,
    state: Mutex<Buckets>,
}

#[derive(Clone, Copy)]
struct Rate {
    burst: f64,
    per_sec: f64,
}

impl Rate {
    /// `per_minute = 0` turns the limit off. The burst is the calls of 10 seconds, at least 1.
    fn new(per_minute: u32) -> (Option<Self>, u32) {
        let burst = per_minute.div_ceil(6);
        let rate = (per_minute > 0).then(|| Self {
            burst: f64::from(burst),
            per_sec: f64::from(per_minute) / 60.0,
        });
        (rate, burst)
    }
}

struct Buckets {
    by_token: HashMap<i64, Bucket>,
    by_user: HashMap<i64, Bucket>,
    last_prune: Instant,
}

struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl RateLimiter {
    pub fn new(token_per_minute: u32, user_per_minute: u32) -> Self {
        let (token_rate, token_burst) = Rate::new(token_per_minute);
        let (user_rate, user_burst) = Rate::new(user_per_minute);
        Self {
            limits: Limits {
                rate_limit_per_minute: token_per_minute,
                rate_limit_burst: token_burst,
                user_rate_limit_per_minute: user_per_minute,
                user_rate_limit_burst: user_burst,
            },
            token_rate,
            user_rate,
            state: Mutex::new(Buckets {
                by_token: HashMap::new(),
                by_user: HashMap::new(),
                last_prune: Instant::now(),
            }),
        }
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Takes one call from the token bucket and one from the user bucket, or tells how long
    /// to wait. When one bucket refuses, the other bucket keeps its calls.
    pub fn check(&self, token_id: i64, user_id: i64) -> Result<(), RateLimited> {
        if self.token_rate.is_none() && self.user_rate.is_none() {
            return Ok(());
        }
        let now = Instant::now();
        // A panic while the lock is held cannot leave the maps in a bad state, so a poisoned
        // lock is safe to use.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Buckets {
            by_token,
            by_user,
            last_prune,
        } = &mut *state;
        if now.duration_since(*last_prune) >= PRUNE_INTERVAL {
            *last_prune = now;
            prune(by_token, self.token_rate, now);
            prune(by_user, self.user_rate, now);
        }
        let token = self
            .token_rate
            .map(|rate| refill(by_token, token_id, rate, now));
        let user = self
            .user_rate
            .map(|rate| refill(by_user, user_id, rate, now));
        let token_wait = token.as_ref().and_then(|(rate, bucket)| bucket.wait(*rate));
        let user_wait = user.as_ref().and_then(|(rate, bucket)| bucket.wait(*rate));
        // When both refuse, the longer wait is the time until the call can pass.
        match (token_wait, user_wait) {
            (Some(t), Some(u)) if u > t => {
                return Err(RateLimited::Account {
                    retry_after_secs: u,
                });
            }
            (Some(t), _) => {
                return Err(RateLimited::Token {
                    retry_after_secs: t,
                });
            }
            (None, Some(u)) => {
                return Err(RateLimited::Account {
                    retry_after_secs: u,
                });
            }
            (None, None) => {}
        }
        for (_, bucket) in token.into_iter().chain(user) {
            bucket.tokens -= 1.0;
        }
        Ok(())
    }

    /// The number of token and user buckets in memory. Tests use it to check that idle
    /// buckets go away.
    pub fn bucket_count(&self) -> (usize, usize) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (state.by_token.len(), state.by_user.len())
    }
}

/// A full bucket acts the same as a new bucket, so removing it changes nothing.
fn prune(buckets: &mut HashMap<i64, Bucket>, rate: Option<Rate>, now: Instant) {
    if let Some(rate) = rate {
        buckets.retain(|_, bucket| bucket.refilled(rate, now) < rate.burst);
    }
}

fn refill(
    buckets: &mut HashMap<i64, Bucket>,
    id: i64,
    rate: Rate,
    now: Instant,
) -> (Rate, &mut Bucket) {
    let bucket = buckets.entry(id).or_insert(Bucket {
        tokens: rate.burst,
        updated: now,
    });
    bucket.tokens = bucket.refilled(rate, now);
    bucket.updated = now;
    (rate, bucket)
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/limits", get(limits))
}

async fn limits(State(state): State<AppState>, _auth: AuthUser) -> Json<Limits> {
    Json(state.rate_limit.limits())
}

impl Bucket {
    fn refilled(&self, rate: Rate, now: Instant) -> f64 {
        let elapsed = now.duration_since(self.updated).as_secs_f64();
        (self.tokens + elapsed * rate.per_sec).min(rate.burst)
    }

    /// Whole seconds until the bucket has one call again, or `None` when it has one now.
    fn wait(&self, rate: Rate) -> Option<u64> {
        (self.tokens < 1.0).then(|| {
            let wait = (1.0 - self.tokens) / rate.per_sec;
            (wait.ceil() as u64).max(1)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A limiter with only the token limit on.
    fn per_token(per_minute: u32) -> RateLimiter {
        RateLimiter::new(per_minute, 0)
    }

    fn take(limiter: &RateLimiter, token_id: i64, count: usize) -> usize {
        take_for(limiter, token_id, 1, count)
    }

    fn take_for(limiter: &RateLimiter, token_id: i64, user_id: i64, count: usize) -> usize {
        (0..count)
            .filter(|_| limiter.check(token_id, user_id).is_ok())
            .count()
    }

    #[tokio::test(start_paused = true)]
    async fn calls_over_the_burst_are_refused() {
        let limiter = per_token(60);
        assert_eq!(take(&limiter, 1, 10), 10);
        assert_eq!(
            limiter.check(1, 1),
            Err(RateLimited::Token {
                retry_after_secs: 1
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn bucket_refills_over_time() {
        let limiter = per_token(60);
        take(&limiter, 1, 10);
        assert!(limiter.check(1, 1).is_err());

        tokio::time::advance(Duration::from_secs(3)).await;
        assert_eq!(take(&limiter, 1, 10), 3);

        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(take(&limiter, 1, 20), 10);
    }

    #[tokio::test(start_paused = true)]
    async fn steady_rate_is_one_call_per_refill_step() {
        let limiter = per_token(60);
        take(&limiter, 1, 10);
        for _ in 0..60 {
            tokio::time::advance(Duration::from_secs(1)).await;
            assert_eq!(take(&limiter, 1, 10), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn retry_time_matches_the_refill() {
        let limiter = per_token(30);
        take(&limiter, 1, 5);
        assert_eq!(
            limiter.check(1, 1).map_err(|e| e.retry_after_secs()),
            Err(2)
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(
            limiter.check(1, 1).map_err(|e| e.retry_after_secs()),
            Err(1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn each_token_has_its_own_bucket() {
        let limiter = per_token(60);
        assert_eq!(take(&limiter, 1, 20), 10);
        assert_eq!(take(&limiter, 2, 20), 10);
    }

    #[tokio::test(start_paused = true)]
    async fn user_limit_is_shared_by_the_tokens_of_a_user() {
        // The user burst is 20 calls, the token burst is 10 calls.
        let limiter = RateLimiter::new(60, 120);
        assert_eq!(take_for(&limiter, 1, 1, 20), 10);
        assert_eq!(take_for(&limiter, 2, 1, 5), 5);
        assert_eq!(take_for(&limiter, 3, 1, 20), 5);
        assert_eq!(
            limiter.check(3, 1),
            Err(RateLimited::Account {
                retry_after_secs: 1
            })
        );
        // A token of another user has its own user bucket.
        assert_eq!(take_for(&limiter, 4, 2, 20), 10);
    }

    #[tokio::test(start_paused = true)]
    async fn refused_call_does_not_use_the_other_bucket() {
        let limiter = RateLimiter::new(60, 60);
        // Token 1 uses the full user bucket of 10 calls.
        assert_eq!(take_for(&limiter, 1, 1, 10), 10);
        // The user bucket refuses token 2, so token 2 keeps its 10 calls.
        assert_eq!(take_for(&limiter, 2, 1, 5), 0);
        assert_eq!(take_for(&limiter, 2, 2, 20), 10);

        // The token bucket refuses token 2, so user 3 keeps its 10 calls.
        assert_eq!(take_for(&limiter, 2, 3, 5), 0);
        assert_eq!(take_for(&limiter, 5, 3, 20), 10);
    }

    #[tokio::test(start_paused = true)]
    async fn longer_wait_wins_when_both_limits_refuse() {
        // Token: 6 per minute, burst 1. User: 60 per minute, burst 10.
        let limiter = RateLimiter::new(6, 60);
        for token_id in 1..=10 {
            assert_eq!(take_for(&limiter, token_id, 1, 1), 1);
        }
        assert_eq!(
            limiter.check(1, 1),
            Err(RateLimited::Token {
                retry_after_secs: 10
            })
        );

        // Token: 12 per minute, burst 2. User: 6 per minute, burst 1.
        let limiter = RateLimiter::new(12, 6);
        assert_eq!(take_for(&limiter, 1, 2, 2), 1);
        assert_eq!(take_for(&limiter, 1, 3, 1), 1);
        assert_eq!(take_for(&limiter, 2, 1, 1), 1);
        assert_eq!(
            limiter.check(1, 1),
            Err(RateLimited::Account {
                retry_after_secs: 10
            })
        );
    }

    #[test]
    fn limits_match_the_bucket() {
        let limits = RateLimiter::new(60, 300).limits();
        assert_eq!(limits.rate_limit_per_minute, 60);
        assert_eq!(limits.rate_limit_burst, 10);
        assert_eq!(limits.user_rate_limit_per_minute, 300);
        assert_eq!(limits.user_rate_limit_burst, 50);
        let limits = RateLimiter::new(1, 0).limits();
        assert_eq!(limits.rate_limit_burst, 1);
        assert_eq!(limits.user_rate_limit_burst, 0);
        assert_eq!(RateLimiter::new(0, 1).limits().rate_limit_burst, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn zero_turns_both_limits_off() {
        let limiter = RateLimiter::new(0, 0);
        assert_eq!(take(&limiter, 1, 10_000), 10_000);
        assert_eq!(limiter.bucket_count(), (0, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn zero_turns_only_the_token_limit_off() {
        let limiter = RateLimiter::new(0, 60);
        assert_eq!(take_for(&limiter, 1, 1, 20), 10);
        assert_eq!(take_for(&limiter, 2, 1, 20), 0);
        assert_eq!(limiter.bucket_count(), (0, 1));
    }

    #[tokio::test(start_paused = true)]
    async fn zero_turns_only_the_user_limit_off() {
        let limiter = RateLimiter::new(6, 0);
        assert_eq!(take_for(&limiter, 1, 1, 20), 1);
        assert_eq!(take_for(&limiter, 2, 1, 20), 1);
        assert_eq!(limiter.bucket_count(), (2, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn idle_buckets_are_removed() {
        let limiter = RateLimiter::new(60, 120);
        take_for(&limiter, 1, 1, 5);
        take_for(&limiter, 2, 2, 5);
        assert_eq!(limiter.bucket_count(), (2, 2));

        tokio::time::advance(PRUNE_INTERVAL).await;
        // The check of token 3 starts the removal. The other buckets are full again.
        take_for(&limiter, 3, 3, 1);
        assert_eq!(limiter.bucket_count(), (1, 1));
    }

    #[tokio::test(start_paused = true)]
    async fn busy_buckets_are_kept() {
        let limiter = RateLimiter::new(6, 6);
        take_for(&limiter, 1, 1, 1);
        tokio::time::advance(PRUNE_INTERVAL - Duration::from_secs(1)).await;
        take_for(&limiter, 1, 1, 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        take_for(&limiter, 2, 2, 1);
        assert_eq!(limiter.bucket_count(), (2, 2));
    }
}
