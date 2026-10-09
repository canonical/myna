//! The supervision rules, pure: events in, actions out. The caller owns the
//! clock, the timers and the processes. The restart policy is the GNOME
//! extension's (`extensions/myna-shell/respawn.js`).

/// First retry delay; each further retry doubles it.
pub const BASE_BACKOFF_MS: u64 = 500;
pub const MAX_BACKOFF_MS: u64 = 30_000;
/// Consecutive failures tolerated before going dormant.
pub const RESTART_BUDGET: u32 = 5;
/// A run this long clears the tally.
pub const HEALTHY_UPTIME_MS: u64 = 60_000;
/// How long a terminated HUD may take before it is killed.
pub const TERM_GRACE_MS: u64 = 3_000;
/// The HUD's exit status for "this session cannot host me".
pub const EXIT_CANNOT_HOST: i32 = 78;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The daemon owns the bus name.
    NameOwned,
    /// The daemon dropped the bus name (or there never was one).
    NameLost,
    /// The HUD is gone. `code` is its exit status, None when a signal ended it.
    Exited { code: Option<i32>, now_ms: u64 },
    /// The HUD could not be started at all.
    SpawnFailed { now_ms: u64 },
    /// The restart delay `Action::ArmRestart` asked for has passed.
    RestartDue,
    /// The grace `Action::ArmKill` asked for has passed.
    KillDue,
    /// Spawn happened at this time (the clock for uptime).
    Spawned { now_ms: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Spawn,
    Terminate,
    Kill,
    ArmRestart(u64),
    CancelRestart,
    ArmKill(u64),
    CancelKill,
    /// Logged once per cause.
    Log(Note),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Note {
    CannotHost,
    Dormant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Child {
    None,
    Running {
        since_ms: u64,
    },
    /// Asked to stop; `then_start` when the name came back meanwhile.
    Stopping {
        then_start: bool,
    },
    Backoff,
}

#[derive(Debug)]
pub struct Machine {
    owned: bool,
    child: Child,
    failures: u32,
    /// The HUD said this session cannot host it.
    refused: bool,
    dormant: bool,
    grace_ms: u64,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub fn new() -> Self {
        Self {
            owned: false,
            child: Child::None,
            failures: 0,
            refused: false,
            dormant: false,
            grace_ms: TERM_GRACE_MS,
        }
    }

    /// How long a terminated HUD may take before it is killed.
    pub fn with_grace(mut self, grace_ms: u64) -> Self {
        self.grace_ms = grace_ms;
        self
    }

    /// Whether a HUD process exists (running or being stopped).
    pub fn has_child(&self) -> bool {
        matches!(self.child, Child::Running { .. } | Child::Stopping { .. })
    }

    pub fn step(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::NameOwned => self.name_owned(),
            Event::NameLost => self.name_lost(),
            Event::Spawned { now_ms } => {
                self.child = Child::Running { since_ms: now_ms };
                vec![]
            }
            Event::Exited { code, now_ms } => self.exited(code, now_ms),
            Event::SpawnFailed { now_ms } => self.exited(None, now_ms),
            Event::RestartDue => {
                if self.child == Child::Backoff && self.owned {
                    self.child = Child::None;
                    vec![Action::Spawn]
                } else {
                    vec![]
                }
            }
            Event::KillDue => {
                if matches!(self.child, Child::Stopping { .. }) {
                    vec![Action::Kill]
                } else {
                    vec![]
                }
            }
        }
    }

    fn startable(&self) -> bool {
        self.owned && !self.refused && !self.dormant
    }

    fn name_owned(&mut self) -> Vec<Action> {
        if self.owned {
            return vec![];
        }
        self.owned = true;
        match self.child {
            Child::None if self.startable() => vec![Action::Spawn],
            Child::Stopping { .. } => {
                self.child = Child::Stopping { then_start: true };
                vec![]
            }
            _ => vec![],
        }
    }

    fn name_lost(&mut self) -> Vec<Action> {
        if !self.owned {
            return vec![];
        }
        self.owned = false;
        // A new daemon lifetime is a fresh incident.
        self.failures = 0;
        self.dormant = false;
        match self.child {
            Child::Running { .. } => {
                self.child = Child::Stopping { then_start: false };
                vec![Action::Terminate, Action::ArmKill(self.grace_ms)]
            }
            Child::Stopping { .. } => {
                self.child = Child::Stopping { then_start: false };
                vec![]
            }
            Child::Backoff => {
                self.child = Child::None;
                vec![Action::CancelRestart]
            }
            Child::None => vec![],
        }
    }

    /// One more consecutive failure: back off, or give up.
    fn fail(&mut self) -> Vec<Action> {
        self.failures += 1;
        if self.failures > RESTART_BUDGET {
            self.dormant = true;
            return vec![Action::Log(Note::Dormant)];
        }
        self.child = Child::Backoff;
        let delay = BASE_BACKOFF_MS
            .saturating_mul(1 << (self.failures - 1))
            .min(MAX_BACKOFF_MS);
        vec![Action::ArmRestart(delay)]
    }

    fn exited(&mut self, code: Option<i32>, now_ms: u64) -> Vec<Action> {
        match self.child {
            Child::Stopping { then_start } => {
                self.failures = 0;
                self.child = Child::None;
                let mut actions = vec![Action::CancelKill];
                if then_start && self.startable() {
                    actions.push(Action::Spawn);
                }
                actions
            }
            Child::Running { since_ms } => {
                self.child = Child::None;
                if code == Some(EXIT_CANNOT_HOST) {
                    self.refused = true;
                    return vec![Action::Log(Note::CannotHost)];
                }
                let uptime = now_ms.saturating_sub(since_ms);
                if uptime >= HEALTHY_UPTIME_MS {
                    self.failures = 0;
                }
                self.fail()
            }
            // A spawn that failed before `Spawned` arrived.
            Child::None if self.startable() => self.fail(),
            Child::None | Child::Backoff => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running() -> Machine {
        let mut m = Machine::new();
        assert_eq!(m.step(Event::NameOwned), [Action::Spawn]);
        m.step(Event::Spawned { now_ms: 0 });
        m
    }

    fn crash(m: &mut Machine, now_ms: u64) -> Vec<Action> {
        m.step(Event::Exited {
            code: Some(1),
            now_ms,
        })
    }

    #[test]
    fn nothing_runs_without_the_name() {
        let mut m = Machine::new();
        assert!(m.step(Event::NameLost).is_empty());
        assert!(!m.has_child());
    }

    #[test]
    fn the_name_appearing_starts_the_hud_once() {
        let mut m = Machine::new();
        assert_eq!(m.step(Event::NameOwned), [Action::Spawn]);
        assert!(m.step(Event::NameOwned).is_empty());
    }

    #[test]
    fn the_name_vanishing_terminates_then_kills_after_the_grace() {
        let mut m = running();
        assert_eq!(
            m.step(Event::NameLost),
            [Action::Terminate, Action::ArmKill(TERM_GRACE_MS)]
        );
        assert_eq!(m.step(Event::KillDue), [Action::Kill]);
        assert_eq!(
            m.step(Event::Exited {
                code: None,
                now_ms: 5000
            }),
            [Action::CancelKill]
        );
        assert!(!m.has_child());
    }

    #[test]
    fn a_hud_that_exits_on_term_is_not_respawned() {
        let mut m = running();
        m.step(Event::NameLost);
        let actions = m.step(Event::Exited {
            code: Some(0),
            now_ms: 10,
        });
        assert_eq!(actions, [Action::CancelKill]);
    }

    #[test]
    fn the_grace_is_configurable() {
        let mut m = Machine::new().with_grace(7);
        m.step(Event::NameOwned);
        m.step(Event::Spawned { now_ms: 0 });
        assert_eq!(
            m.step(Event::NameLost),
            [Action::Terminate, Action::ArmKill(7)]
        );
    }

    #[test]
    fn a_late_kill_timer_is_ignored() {
        let mut m = running();
        assert!(m.step(Event::KillDue).is_empty());
    }

    #[test]
    fn a_crash_backs_off_and_doubles() {
        let mut m = running();
        assert_eq!(crash(&mut m, 100), [Action::ArmRestart(500)]);
        assert_eq!(m.step(Event::RestartDue), [Action::Spawn]);
        m.step(Event::Spawned { now_ms: 600 });
        assert_eq!(crash(&mut m, 700), [Action::ArmRestart(1000)]);
        assert_eq!(m.step(Event::RestartDue), [Action::Spawn]);
        m.step(Event::Spawned { now_ms: 1700 });
        assert_eq!(crash(&mut m, 1800), [Action::ArmRestart(2000)]);
    }

    #[test]
    fn a_signal_death_counts_as_a_crash() {
        let mut m = running();
        let actions = m.step(Event::Exited {
            code: None,
            now_ms: 10,
        });
        assert_eq!(actions, [Action::ArmRestart(500)]);
    }

    #[test]
    fn backoff_is_capped() {
        let mut m = running();
        let mut delays = vec![];
        for i in 0..u64::from(RESTART_BUDGET) {
            if let [Action::ArmRestart(d)] = crash(&mut m, i).as_slice() {
                delays.push(*d);
            }
            m.step(Event::RestartDue);
            m.step(Event::Spawned { now_ms: i });
        }
        assert_eq!(delays, [500, 1000, 2000, 4000, 8000]);
        assert!(delays.iter().all(|d| *d <= MAX_BACKOFF_MS));
    }

    #[test]
    fn the_budget_runs_out_into_dormancy_logged_once() {
        let mut m = running();
        for i in 0..u64::from(RESTART_BUDGET) {
            assert!(matches!(
                crash(&mut m, i).as_slice(),
                [Action::ArmRestart(_)]
            ));
            m.step(Event::RestartDue);
            m.step(Event::Spawned { now_ms: i });
        }
        assert_eq!(crash(&mut m, 9), [Action::Log(Note::Dormant)]);
        // The name flapping does not restart a dormant HUD...
        assert!(m.step(Event::NameOwned).is_empty());
        assert!(!m.has_child());
    }

    #[test]
    fn a_new_daemon_lifetime_clears_dormancy() {
        let mut m = running();
        for i in 0..u64::from(RESTART_BUDGET) {
            crash(&mut m, i);
            m.step(Event::RestartDue);
            m.step(Event::Spawned { now_ms: i });
        }
        assert_eq!(crash(&mut m, 9), [Action::Log(Note::Dormant)]);
        m.step(Event::NameLost);
        assert_eq!(m.step(Event::NameOwned), [Action::Spawn]);
    }

    #[test]
    fn a_healthy_run_resets_the_tally() {
        let mut m = running();
        for i in 0..4u64 {
            crash(&mut m, i);
            m.step(Event::RestartDue);
            m.step(Event::Spawned { now_ms: i });
        }
        // Up for a minute, then a crash: failure number one again.
        assert_eq!(
            crash(&mut m, HEALTHY_UPTIME_MS + 3),
            [Action::ArmRestart(500)]
        );
    }

    #[test]
    fn just_under_healthy_does_not_reset() {
        let mut m = running();
        crash(&mut m, 1);
        m.step(Event::RestartDue);
        m.step(Event::Spawned { now_ms: 0 });
        assert_eq!(
            crash(&mut m, HEALTHY_UPTIME_MS - 1),
            [Action::ArmRestart(1000)]
        );
    }

    #[test]
    fn exit_78_stops_for_good_and_logs_once() {
        let mut m = running();
        let actions = m.step(Event::Exited {
            code: Some(EXIT_CANNOT_HOST),
            now_ms: 5,
        });
        assert_eq!(actions, [Action::Log(Note::CannotHost)]);
        m.step(Event::NameLost);
        assert!(m.step(Event::NameOwned).is_empty());
        assert!(!m.has_child());
    }

    #[test]
    fn exit_78_while_being_stopped_is_just_a_stop() {
        let mut m = running();
        m.step(Event::NameLost);
        let actions = m.step(Event::Exited {
            code: Some(EXIT_CANNOT_HOST),
            now_ms: 5,
        });
        assert_eq!(actions, [Action::CancelKill]);
        assert_eq!(m.step(Event::NameOwned), [Action::Spawn]);
    }

    #[test]
    fn the_name_vanishing_cancels_a_pending_restart() {
        let mut m = running();
        crash(&mut m, 1);
        assert_eq!(m.step(Event::NameLost), [Action::CancelRestart]);
        assert!(m.step(Event::RestartDue).is_empty());
    }

    #[test]
    fn a_stale_restart_timer_after_losing_the_name_spawns_nothing() {
        let mut m = running();
        crash(&mut m, 1);
        m.step(Event::NameLost);
        m.step(Event::NameOwned);
        // NameOwned spawned afresh (Child::None); a late RestartDue is moot.
        assert!(m.step(Event::RestartDue).is_empty());
    }

    #[test]
    fn the_name_returning_while_stopping_starts_a_fresh_hud_after_the_exit() {
        let mut m = running();
        m.step(Event::NameLost);
        assert!(m.step(Event::NameOwned).is_empty());
        let actions = m.step(Event::Exited {
            code: Some(0),
            now_ms: 7,
        });
        assert_eq!(actions, [Action::CancelKill, Action::Spawn]);
    }

    #[test]
    fn losing_the_name_again_while_stopping_cancels_the_restart_of_it() {
        let mut m = running();
        m.step(Event::NameLost);
        m.step(Event::NameOwned);
        assert!(m.step(Event::NameLost).is_empty());
        let actions = m.step(Event::Exited {
            code: Some(0),
            now_ms: 7,
        });
        assert_eq!(actions, [Action::CancelKill]);
    }

    #[test]
    fn a_failed_spawn_backs_off_like_a_crash() {
        let mut m = Machine::new();
        assert_eq!(m.step(Event::NameOwned), [Action::Spawn]);
        assert_eq!(
            m.step(Event::SpawnFailed { now_ms: 0 }),
            [Action::ArmRestart(500)]
        );
        assert_eq!(m.step(Event::RestartDue), [Action::Spawn]);
    }

    #[test]
    fn failed_spawns_run_out_into_dormancy_too() {
        let mut m = Machine::new();
        m.step(Event::NameOwned);
        for _ in 0..RESTART_BUDGET {
            assert!(matches!(
                m.step(Event::SpawnFailed { now_ms: 0 }).as_slice(),
                [Action::ArmRestart(_)]
            ));
            m.step(Event::RestartDue);
        }
        assert_eq!(
            m.step(Event::SpawnFailed { now_ms: 0 }),
            [Action::Log(Note::Dormant)]
        );
    }

    #[test]
    fn a_stray_exit_with_no_child_is_ignored() {
        let mut m = Machine::new();
        assert!(m
            .step(Event::Exited {
                code: Some(1),
                now_ms: 0
            })
            .is_empty());
    }

    #[test]
    fn losing_the_name_twice_is_idempotent() {
        let mut m = running();
        m.step(Event::NameLost);
        assert!(m.step(Event::NameLost).is_empty());
    }
}
