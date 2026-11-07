//! A request's cancellation/deadline is shared by SQL, queueing and storage waits.
use std::time::{Duration, Instant, SystemTime};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum Stop {
    #[error("request cancelled")]
    Cancelled,
    #[error("request deadline expired or wall clock moved backwards")]
    Deadline,
}
#[derive(Clone, Debug)]
pub struct RequestContext {
    started: Instant,
    wall_started: SystemTime,
    budget: Option<Duration>,
    token: CancellationToken,
}
impl RequestContext {
    pub fn new(budget: Duration) -> Self {
        Self {
            started: Instant::now(),
            wall_started: SystemTime::now(),
            budget: Some(budget),
            token: CancellationToken::new(),
        }
    }
    /// A cancellable context without an execution deadline.
    pub fn unbounded() -> Self {
        Self {
            started: Instant::now(),
            wall_started: SystemTime::now(),
            budget: None,
            token: CancellationToken::new(),
        }
    }
    pub fn with_max_duration(&self, budget: Duration) -> Self {
        let mut child = self.clone();
        child.budget = Some(child.budget.map_or(budget, |existing| existing.min(budget)));
        child
    }
    pub fn cancel(&self) {
        self.token.cancel();
    }
    pub fn check(&self) -> Result<(), Stop> {
        self.check_at(Instant::now(), SystemTime::now())
    }
    fn check_at(&self, now: Instant, wall: SystemTime) -> Result<(), Stop> {
        if self.token.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        let Some(budget) = self.budget else {
            return Ok(());
        };
        // Both clocks: monotonic time may exclude system suspension on some platforms.
        let elapsed = wall
            .duration_since(self.wall_started)
            .map_err(|_| Stop::Deadline)?;
        if now.duration_since(self.started) >= budget || elapsed >= budget {
            Err(Stop::Deadline)
        } else {
            Ok(())
        }
    }
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unbounded_context_ignores_clocks_but_can_be_bounded_and_cancelled() {
        let ctx = RequestContext::unbounded();
        assert!(ctx
            .check_at(
                ctx.started + Duration::from_secs(86400),
                ctx.wall_started + Duration::from_secs(86400)
            )
            .is_ok());
        assert!(ctx
            .check_at(ctx.started, ctx.wall_started - Duration::from_secs(1))
            .is_ok());
        let child = ctx.with_max_duration(Duration::from_secs(1));
        assert!(child
            .check_at(
                child.started + Duration::from_secs(2),
                child.wall_started + Duration::from_secs(2)
            )
            .is_err());
        ctx.cancel();
        assert_eq!(ctx.check(), Err(Stop::Cancelled));
        assert_eq!(child.check(), Err(Stop::Cancelled));
    }
    #[test]
    fn expired_suspended_or_untrusted_clock_cannot_resume() {
        let ctx = RequestContext::new(Duration::from_secs(1));
        assert!(ctx
            .check_at(ctx.started, ctx.wall_started + Duration::from_secs(2))
            .is_err());
        assert!(ctx
            .check_at(ctx.started, ctx.wall_started - Duration::from_secs(1))
            .is_err());
        assert!(ctx
            .check_at(ctx.started + Duration::from_secs(2), ctx.wall_started)
            .is_err());
        ctx.cancel();
        assert_eq!(ctx.check(), Err(Stop::Cancelled));
    }
}
