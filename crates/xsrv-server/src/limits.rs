// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Limits for the pages: how many may be open, and how fast one may send input.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub const MAX_PAGES: usize = 16;
pub const MAX_INPUT_PER_SECOND: u32 = 3000;
/// The page sends one resize for each 200 ms of window dragging, so this is generous.
pub const MAX_RESIZE_PER_SECOND: u32 = 10;
const PERIOD: Duration = Duration::from_secs(1);

/// How many pages are open. A page holds a slot until its guard is dropped.
pub struct PageSlots {
    open: AtomicUsize,
}

pub struct PageGuard<'a> {
    slots: &'a PageSlots,
}

impl PageSlots {
    pub const fn new() -> Self {
        Self {
            open: AtomicUsize::new(0),
        }
    }

    pub fn try_acquire(&self, max: usize) -> Option<PageGuard<'_>> {
        let mut open = self.open.load(Ordering::Acquire);
        loop {
            if open >= max {
                return None;
            }
            match self
                .open
                .compare_exchange(open, open + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Some(PageGuard { slots: self }),
                Err(current) => open = current,
            }
        }
    }

    #[cfg(test)]
    fn open(&self) -> usize {
        self.open.load(Ordering::Acquire)
    }
}

impl Drop for PageGuard<'_> {
    fn drop(&mut self) {
        self.slots.open.fetch_sub(1, Ordering::AcqRel);
    }
}

/// At most `limit` messages in each second.
pub struct RateLimit {
    limit: u32,
    period_start: Instant,
    count: u32,
}

impl RateLimit {
    pub fn new(limit: u32, now: Instant) -> Self {
        Self {
            limit,
            period_start: now,
            count: 0,
        }
    }

    pub fn allow(&mut self, now: Instant) -> bool {
        if now.duration_since(self.period_start) >= PERIOD {
            self.period_start = now;
            self.count = 0;
        }
        self.count += 1;
        self.count <= self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_given_and_returned() {
        let slots = PageSlots::new();
        let first = slots.try_acquire(2).unwrap();
        let second = slots.try_acquire(2).unwrap();
        assert!(slots.try_acquire(2).is_none(), "only two pages");
        drop(first);
        assert_eq!(slots.open(), 1);
        let third = slots.try_acquire(2).unwrap();
        drop((second, third));
        assert_eq!(slots.open(), 0);
    }

    #[test]
    fn the_rate_limit_allows_a_burst_up_to_the_limit_and_resets_each_second() {
        let start = Instant::now();
        let mut limit = RateLimit::new(3, start);
        assert!([0, 1, 2].iter().all(|_| limit.allow(start)));
        assert!(!limit.allow(start), "the fourth message in the second");
        assert!(!limit.allow(start + Duration::from_millis(999)));
        assert!(limit.allow(start + PERIOD), "a new second begins");
        assert!(limit.allow(start + PERIOD));
    }
}
