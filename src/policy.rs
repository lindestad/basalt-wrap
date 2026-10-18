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
    seen_at: Option<f64>,
    activity: Option<String>,
    last_cache: Option<u64>,
    pub pending: Option<(Resize, Sample)>,
    next_shrink: f64,
    blocked: Vec<(bool, Sample)>,
}
impl Policy {
    pub fn new(min_free: u64, grow_below: u64, delay: f64, poll: f64) -> Self {
        Self {
            min_free,
            grow_below,
            delay,
            poll,
            quiet_since: None,
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
        if busy || self.activity.as_ref() != Some(&activity) {
            self.quiet_since = None;
        }
        self.activity = Some(activity);
    }
    pub fn observe(&mut self, now: f64, s: Sample) -> Option<Resize> {
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
        if s.busy || changed || resized || s.used >= self.grow_below {
            self.quiet_since = None;
        } else if self.quiet_since.is_none() {
            self.quiet_since = Some(now);
        }
        if self.pending.is_some() {
            return None;
        }
        self.blocked.retain(|(_, old)| {
            old.cache == s.cache
                && old.full == s.full
                && old.activity == s.activity
                && old.free.abs_diff(s.free) < 512
        });
        let action = if s.free < self.min_free
            && now >= self.next_shrink
            && !self.blocked.iter().any(|(g, _)| !g)
        {
            Some(Resize {
                grow: false,
                reserve: s.free + 528,
                before: s.cache,
            })
        } else if !s.busy
            && s.used < self.grow_below
            && s.cache < s.full
            && !self.blocked.iter().any(|(g, _)| *g)
            && self.quiet_since.is_some_and(|t| now - t >= self.delay)
            && s.free >= self.min_free + 528
        {
            Some(Resize {
                grow: true,
                reserve: s.free - 528,
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
            self.blocked.push((action.grow, sample));
        }
        changed
    }
    pub fn quiet_left(&self, now: f64) -> Option<f64> {
        self.quiet_since.map(|t| (self.delay - (now - t)).max(0.))
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
}
