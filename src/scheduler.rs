use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::model::Rating;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SchedulerState {
    pub interval_days: u32,
    pub ease: f32,
    pub repetitions: u32,
}

impl Default for SchedulerState {
    fn default() -> Self {
        Self {
            interval_days: 0,
            ease: 2.5,
            repetitions: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ReviewOutcome {
    pub state: SchedulerState,
    pub next_due: DateTime<Utc>,
}

pub trait Scheduler: Send + Sync {
    fn on_review(&self, state: SchedulerState, rating: Rating, now: DateTime<Utc>)
    -> ReviewOutcome;
}

pub struct Sm2;

impl Scheduler for Sm2 {
    fn on_review(
        &self,
        mut state: SchedulerState,
        rating: Rating,
        now: DateTime<Utc>,
    ) -> ReviewOutcome {
        // SuperMemo SM-2 calls this "quality of response" (0–5); we call it
        // `recall_quality` because the user is grading their recall, not the
        // response itself. Consumed by the ease-factor formula below (verbatim
        // from the SM-2 paper). Other schedulers (e.g. FSRS) map the same
        // Rating to different numbers, so this mapping is SM-2-local rather
        // than living on `Rating`.
        let recall_quality: f32 = match rating {
            Rating::Again => 0.0,
            Rating::Hard => 3.0,
            Rating::Good => 4.0,
            Rating::Easy => 5.0,
        };

        if rating == Rating::Again {
            state.repetitions = 0;
            state.interval_days = 1;
        } else {
            state.repetitions += 1;
            state.interval_days = match state.repetitions {
                1 => 1,
                2 => 6,
                _ => ((state.interval_days as f32) * state.ease).round() as u32,
            };
        }

        state.ease = (state.ease
            + (0.1 - (5.0 - recall_quality) * (0.08 + (5.0 - recall_quality) * 0.02)))
            .max(1.3);

        let next_due = now + Duration::days(state.interval_days as i64);
        ReviewOutcome { state, next_due }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sm2_again_resets_interval() {
        let s = Sm2;
        let state = SchedulerState {
            interval_days: 30,
            ease: 2.5,
            repetitions: 5,
        };
        let out = s.on_review(state, Rating::Again, Utc::now());
        assert_eq!(out.state.repetitions, 0);
        assert_eq!(out.state.interval_days, 1);
    }

    #[test]
    fn sm2_first_good_review() {
        let s = Sm2;
        let out = s.on_review(SchedulerState::default(), Rating::Good, Utc::now());
        assert_eq!(out.state.repetitions, 1);
        assert_eq!(out.state.interval_days, 1);
    }

    #[test]
    fn sm2_ease_floor() {
        let s = Sm2;
        let mut state = SchedulerState {
            interval_days: 1,
            ease: 1.3,
            repetitions: 0,
        };
        for _ in 0..5 {
            state = s.on_review(state, Rating::Again, Utc::now()).state;
        }
        assert!(state.ease >= 1.3);
    }
}
