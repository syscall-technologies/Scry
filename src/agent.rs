//! Agent gate shared by the session, the MCP/control API and the viewer: records what the agent
//! is doing and lets the human pause, stop or take over.

use anyhow::{Result, bail};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// A paused agent call gives up after this long, so MCP clients don't hang forever.
const PAUSE_TIMEOUT: Duration = Duration::from_secs(120);
/// Human input pauses the agent only if it acted this recently.
const TAKEOVER_WINDOW: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Running,
    Paused,
    Stopped,
}

#[derive(Clone)]
pub struct Activity {
    pub label: String,
    pub done: usize,
    pub total: usize,
    pub finished: Option<Instant>,
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub mode: Mode,
    pub activity: Option<Activity>,
}

#[derive(Default)]
pub struct AgentControl {
    state: Mutex<Snapshot>,
    changed: Condvar,
}

pub struct ActivityGuard<'a>(&'a AgentControl);

impl AgentControl {
    pub fn snapshot(&self) -> Snapshot {
        self.state.lock().expect("agent lock").clone()
    }

    pub fn set_mode(&self, mode: Mode) {
        self.state.lock().expect("agent lock").mode = mode;
        self.changed.notify_all();
    }

    /// Blocks while paused; fails once stopped. Called before each agent input unit.
    pub fn wait_turn(&self) -> Result<()> {
        let state = self.state.lock().expect("agent lock");
        let (state, _) = self
            .changed
            .wait_timeout_while(state, PAUSE_TIMEOUT, |s| s.mode == Mode::Paused)
            .expect("agent lock");
        match state.mode {
            Mode::Running => Ok(()),
            Mode::Paused => bail!("the user paused agent control for over 2 minutes; try again later"),
            Mode::Stopped => {
                bail!("the user stopped agent control; ask them to click \"Allow agent\" in the viewer")
            }
        }
    }

    /// Marks an agent action as in progress until the guard is dropped.
    pub fn begin(&self, label: String, total: usize) -> ActivityGuard<'_> {
        self.state.lock().expect("agent lock").activity = Some(Activity {
            label,
            done: 0,
            total,
            finished: None,
        });
        ActivityGuard(self)
    }

    /// Local input from the viewer: pauses the agent if it is acting (or just acted).
    pub fn human_input(&self) {
        let mut state = self.state.lock().expect("agent lock");
        let recent = state
            .activity
            .as_ref()
            .is_some_and(|a| a.finished.is_none_or(|t| t.elapsed() < TAKEOVER_WINDOW));
        if state.mode == Mode::Running && recent {
            state.mode = Mode::Paused;
            drop(state);
            self.changed.notify_all();
        }
    }
}

impl ActivityGuard<'_> {
    pub fn advance(&self) {
        if let Some(a) = &mut self.0.state.lock().expect("agent lock").activity {
            a.done += 1;
        }
    }
}

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        if let Some(a) = &mut self.0.state.lock().expect("agent lock").activity {
            a.finished = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn pause_blocks_until_resume_and_stop_refuses() {
        let agent = Arc::new(AgentControl::default());
        agent.set_mode(Mode::Paused);
        let waiter = {
            let agent = Arc::clone(&agent);
            thread::spawn(move || agent.wait_turn())
        };
        thread::sleep(Duration::from_millis(50));
        assert!(!waiter.is_finished());
        agent.set_mode(Mode::Running);
        assert!(waiter.join().unwrap().is_ok());

        agent.set_mode(Mode::Stopped);
        assert!(agent.wait_turn().is_err());
    }

    #[test]
    fn human_input_pauses_only_recent_agent() {
        let agent = AgentControl::default();
        agent.human_input();
        assert_eq!(agent.snapshot().mode, Mode::Running, "no agent activity yet");
        {
            let _a = agent.begin("click".into(), 1);
            agent.human_input();
        }
        assert_eq!(agent.snapshot().mode, Mode::Paused);
    }
}
