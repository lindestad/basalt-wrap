#[derive(
    Clone, Copy, Debug, Default, PartialEq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum GrowthMode {
    #[default]
    Idle,
    Capacity,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub used: u64,
    pub free: u64,
    pub cache: u64,
    pub full: u64,
    pub activity: String,
    pub busy: bool,
}
#[derive(Clone, Debug)]
pub struct Resize {
    pub grow: bool,
    pub reserve: u64,
    pub before: u64,
}
pub struct Policy {
    pub min_free: u64,
    pub grow_below: u64,
    pub delay: f64,
    pub poll: f64,
    pub quiet_since: Option<f64>,
    pub growth_mode: GrowthMode,
    pub growth_paused: bool,
    pub step: u64,
    pub margin: u64,
    pub retry_delay: f64,
    pub latest: Option<Sample>,
    seen_at: Option<f64>,
    activity: Option<String>,
    last_cache: Option<u64>,
    pub pending: Option<(Resize, Sample)>,
    next_shrink: f64,
    blocked: Vec<(bool, Sample, f64)>,
}
impl Policy {
    pub fn new(min_free: u64, grow_below: u64, delay: f64, poll: f64) -> Self {
        Self {
            min_free,
            grow_below,
            delay,
            poll,
            quiet_since: None,
            growth_mode: GrowthMode::Idle,
            growth_paused: false,
            step: 512,
            margin: 16,
            retry_delay: 60.,
            latest: None,
            seen_at: None,
            activity: None,
            last_cache: None,
            pending: None,
            next_shrink: 0.,
            blocked: vec![],
        }
    }
    pub fn interrupt(&mut self) {
        self.quiet_since = None;
        self.seen_at = None;
    }
    pub fn activity(&mut self, activity: String, busy: bool) {
        if self.growth_mode == GrowthMode::Idle
            && (busy || self.activity.as_ref() != Some(&activity))
        {
            self.quiet_since = None;
        }
        self.activity = Some(activity);
    }
    pub fn observe(&mut self, now: f64, s: Sample) -> Option<Resize> {
        self.latest = Some(s.clone());
        if self
            .seen_at
            .is_some_and(|t| now - t > 5f64.max(self.poll * 3.))
        {
            self.quiet_since = None;
        }
        self.seen_at = Some(now);
        let changed = self.activity.as_ref() != Some(&s.activity);
        self.activity = Some(s.activity.clone());
        let resized = self.last_cache.is_some_and(|c| c != s.cache);
        self.last_cache = Some(s.cache);
        if self.growth_paused
            || (self.growth_mode == GrowthMode::Idle && (s.busy || changed))
            || resized
            || s.used >= self.grow_below
            || s.cache >= s.full
            || s.free
                < self
                    .min_free
                    .saturating_add(self.step)
                    .saturating_add(self.margin)
        {
            self.quiet_since = None;
        } else if self.quiet_since.is_none() {
            self.quiet_since = Some(now);
        }
        if self.pending.is_some() {
            return None;
        }
        self.blocked.retain(|(_, old, at)| {
            old.cache == s.cache
                && old.full == s.full
                && (self.growth_mode == GrowthMode::Capacity || old.activity == s.activity)
                && old.free.abs_diff(s.free) < self.step
                && now - at < self.retry_delay
        });
        let action = if s.free < self.min_free
            && now >= self.next_shrink
            && !self.blocked.iter().any(|(g, _, _)| !g)
        {
            Some(Resize {
                grow: false,
                reserve: s.free.saturating_add(self.step).saturating_add(self.margin),
                before: s.cache,
            })
        } else if !self.growth_paused
            && (!s.busy || self.growth_mode == GrowthMode::Capacity)
            && s.used < self.grow_below
            && s.cache < s.full
            && !self.blocked.iter().any(|(g, _, _)| *g)
            && self.quiet_since.is_some_and(|t| now - t >= self.delay)
            && s.free
                >= self
                    .min_free
                    .saturating_add(self.step)
                    .saturating_add(self.margin)
        {
            Some(Resize {
                grow: true,
                reserve: s.free - self.step - self.margin,
                before: s.cache,
            })
        } else {
            None
        };
        if let Some(action) = &action {
            self.pending = Some((action.clone(), s));
            self.quiet_since = None;
        }
        action
    }
    pub fn complete(&mut self, now: f64, cache: Option<u64>) -> bool {
        let Some((action, sample)) = self.pending.take() else {
            return false;
        };
        self.quiet_since = None;
        self.next_shrink = now + self.poll;
        if let Some(cache) = cache {
            self.last_cache = Some(cache);
        }
        let changed = cache.is_some_and(|c| {
            if action.grow {
                c > action.before
            } else {
                c < action.before
            }
        });
        if !changed {
            self.blocked.push((action.grow, sample, now));
        }
        changed
    }
    pub fn quiet_left(&self, now: f64) -> Option<f64> {
        self.quiet_since.map(|t| (self.delay - (now - t)).max(0.))
    }
    pub fn status(&self, now: f64) -> String {
        if let Some((action, _)) = &self.pending {
            return format!(
                "{} queued · waiting for Strata",
                if action.grow { "Grow" } else { "Shrink" }
            );
        }
        let Some(s) = &self.latest else {
            return "Waiting for GPU telemetry".into();
        };
        if s.free < self.min_free {
            return "Pressure · waiting to shrink".into();
        }
        if self.growth_paused {
            return "Growth paused · pressure shrinking enabled".into();
        }
        if s.cache >= s.full {
            return "Holding · full expert cache".into();
        }
        if self.growth_mode == GrowthMode::Idle && s.busy {
            return "Holding · waiting for inference to finish".into();
        }
        if s.used >= self.grow_below {
            return format!(
                "Holding · GPU use ≥{:.1} GiB",
                self.grow_below as f64 / 1024.
            );
        }
        if let Some((_, _, at)) = self.blocked.iter().find(|(grow, _, _)| *grow) {
            return format!(
                "Grow retry in {:.0}s · engine made no growth",
                (self.retry_delay - (now - at)).max(0.).ceil()
            );
        }
        if s.free
            < self
                .min_free
                .saturating_add(self.step)
                .saturating_add(self.margin)
        {
            return "Holding · not enough room for a growth step".into();
        }
        if let Some(left) = self.quiet_left(now) {
            return format!(
                "Growing in {:.0}s · {}",
                left.ceil(),
                if self.growth_mode == GrowthMode::Idle {
                    "quiet period"
                } else {
                    "spare VRAM"
                }
            );
        }
        "Holding · watching free memory".into()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Sample {
        Sample {
            used: 13000,
            free: 3200,
            cache: 6000,
            full: 8000,
            activity: "1".into(),
            busy: false,
        }
    }
    #[test]
    fn capacity_mode_reproduces_continuous_session_and_queues_one_grow() {
        // The user's live state: 13.4 GiB used, requests arriving continuously,
        // 4 GiB of a 5.2 GiB expert cache allocated. Idle mode never earns a grow.
        for mode in [GrowthMode::Idle, GrowthMode::Capacity] {
            let mut p = Policy::new(1024, 14336, 60., 2.);
            p.growth_mode = mode;
            let mut s = Sample {
                used: 13735,
                free: 2568,
                cache: 4096,
                full: 5302,
                activity: "0".into(),
                busy: true,
            };
            for t in (0..60).step_by(2) {
                s.activity = t.to_string();
                p.activity(s.activity.clone(), true);
                assert!(p.observe(t as f64, s.clone()).is_none());
            }
            s.activity = "60".into();
            p.activity(s.activity.clone(), true);
            let action = p.observe(60., s.clone());
            if mode == GrowthMode::Capacity {
                assert_eq!(action.unwrap().reserve, 2040);
                assert!(p.status(60.).contains("Grow queued"));
                assert!(p.observe(62., s).is_none());
            } else {
                assert!(action.is_none());
            }
        }
    }
    #[test]
    fn capacity_mode_resets_for_pressure_and_requires_new_time_after_each_grow() {
        let mut p = Policy::new(1024, 14336, 6., 2.);
        p.growth_mode = GrowthMode::Capacity;
        let mut s = sample();
        s.busy = true;
        for t in [0., 2., 4.] {
            assert!(p.observe(t, s.clone()).is_none());
        }
        s.used = 14400;
        assert!(p.observe(6., s.clone()).is_none());
        s.used = 13000;
        assert!(p.observe(8., s.clone()).is_none());
        for t in [10., 12.] {
            assert!(p.observe(t, s.clone()).is_none());
        }
        assert!(p.observe(14., s.clone()).is_some());
        assert!(p.complete(15., Some(6512)));
        s.cache = 6512;
        for t in [16., 18., 20.] {
            assert!(p.observe(t, s.clone()).is_none());
        }
        assert!(p.observe(22., s).is_some());
    }
    #[test]
    fn no_op_retries_are_bounded_even_when_requests_keep_arriving() {
        let mut p = Policy::new(1024, 14336, 0., 2.);
        p.growth_mode = GrowthMode::Capacity;
        p.retry_delay = 6.;
        let mut s = sample();
        s.busy = true;
        assert!(p.observe(0., s.clone()).is_some());
        assert!(!p.complete(1., Some(6000)));
        for t in [2., 4., 6.] {
            s.activity = t.to_string();
            p.activity(s.activity.clone(), true);
            assert!(p.observe(t, s.clone()).is_none());
        }
        assert!(p.observe(8., s).is_some());
    }
    #[test]
    fn configurable_steps_and_margin_preserve_headroom() {
        let mut p = Policy::new(1024, 14336, 0., 2.);
        p.step = 256;
        p.margin = 32;
        assert!(p.observe(0., sample()).is_none());
        assert_eq!(p.observe(2., sample()).unwrap().reserve, 2912);
        p.complete(3., Some(6256));
        let mut s = sample();
        s.free = 1200;
        s.cache = 6256;
        assert!(p.observe(4., s.clone()).is_none());
        assert!(p.status(4.).contains("not enough room"));
        s.free = 500;
        assert_eq!(p.observe(6., s).unwrap().reserve, 788);
    }
    #[test]
    fn shrink_waits_for_confirmation_then_repeats() {
        let mut p = Policy::new(1024, 14336, 60., 2.);
        let mut s = sample();
        s.free = 700;
        assert_eq!(p.observe(0., s.clone()).unwrap().reserve, 1228);
        assert!(p.observe(10., s.clone()).is_none());
        assert!(p.complete(10., Some(5488)));
        s.cache = 5488;
        assert!(p.observe(11., s.clone()).is_none());
        assert!(p.observe(12., s).is_some());
    }
    #[test]
    fn growth_requires_fresh_quiet_period_for_each_step() {
        let mut p = Policy::new(1024, 14336, 60., 2.);
        let mut s = sample();
        p.observe(0., s.clone());
        p.observe(2., s.clone());
        for t in (4..62).step_by(2) {
            assert!(p.observe(t as f64, s.clone()).is_none());
        }
        assert_eq!(p.observe(62., s.clone()).unwrap().reserve, 2672);
        assert!(p.complete(63., Some(6512)));
        s.cache = 6512;
        p.observe(64., s.clone());
        for t in (66..124).step_by(2) {
            assert!(p.observe(t as f64, s.clone()).is_none());
        }
        assert!(p.observe(124., s).is_some());
    }
    #[test]
    fn no_op_and_small_accounting_fluctuations_do_not_spin() {
        let mut p = Policy::new(1024, 14336, 60., 2.);
        let mut s = sample();
        s.free = 700;
        p.observe(0., s.clone());
        assert!(!p.complete(1., Some(6000)));
        s.free = 690;
        for t in (4..50).step_by(2) {
            assert!(p.observe(t as f64, s.clone()).is_none());
        }
        s.free = 180;
        assert!(p.observe(50., s).is_some());
    }
    #[test]
    fn short_inference_and_observation_gap_reset_timer() {
        let mut p = Policy::new(1024, 14336, 6., 2.);
        let mut s = sample();
        p.observe(0., s.clone());
        p.observe(2., s.clone());
        p.observe(4., s.clone());
        p.activity("2".into(), true);
        s.activity = "2".into();
        p.observe(6., s.clone());
        assert!(p.observe(8., s.clone()).is_none());
        assert!(p.observe(20., s).is_none());
    }
    #[test]
    fn full_cache_and_minimum_free_prevent_growth() {
        for s in [
            Sample {
                cache: 8000,
                ..sample()
            },
            Sample {
                free: 1500,
                ..sample()
            },
        ] {
            let mut p = Policy::new(1024, 14336, 0., 2.);
            assert!(p.observe(0., s.clone()).is_none());
            assert!(p.observe(2., s).is_none());
        }
    }
    #[test]
    fn pausing_growth_keeps_pressure_shrinking_enabled() {
        let mut p = Policy::new(1024, 14336, 0., 2.);
        p.growth_mode = GrowthMode::Capacity;
        p.growth_paused = true;
        let mut s = sample();
        assert!(p.observe(0., s.clone()).is_none());
        assert!(p.observe(2., s.clone()).is_none());
        s.free = 500;
        assert!(!p.observe(4., s).unwrap().grow);
    }
}
