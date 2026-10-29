//! What one agent run spent, as its job event tells it: `resolved` for a rebase's conflict,
//! `fix` for failed checks, and `unresolved` or `unfixed` when the run did not do its task. The
//! job writes the message, the dashboard reads it back.

/// Why the agent ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpendTask {
    /// It resolved a conflict in a rebase.
    Rebase,
    /// It fixed failed checks.
    Fix,
}

impl SpendTask {
    /// The job event that tells of the run: whether it did its task, or not.
    pub fn event_kind(self, done: bool) -> &'static str {
        match (self, done) {
            (SpendTask::Rebase, true) => "resolved",
            (SpendTask::Rebase, false) => "unresolved",
            (SpendTask::Fix, true) => "fix",
            (SpendTask::Fix, false) => "unfixed",
        }
    }

    /// Every event that tells of a run.
    pub fn event_kinds() -> [&'static str; 4] {
        [
            SpendTask::Rebase.event_kind(true),
            SpendTask::Rebase.event_kind(false),
            SpendTask::Fix.event_kind(true),
            SpendTask::Fix.event_kind(false),
        ]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spend {
    pub task: SpendTask,
    /// Whether it did its task. A run that gave up, broke, or left a fix that fails still spent.
    pub done: bool,
    /// The conflicted files, comma-separated. Empty for a fix.
    pub files: String,
    pub turns: u32,
    /// `None` when the agent's CLI did not say.
    pub cost_usd: Option<f64>,
    /// `None` in events from before the job named them.
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Spend {
    /// The event's kind.
    pub fn kind(&self) -> &'static str {
        self.task.event_kind(self.done)
    }

    /// The event's message: `a.rs, b.rs in 46 turns, $0.46, by claude-opus-5-5 at high effort`.
    pub fn message(&self) -> String {
        let mut m = String::new();
        if !self.files.is_empty() {
            m.push_str(&format!("{} in ", self.files));
        }
        m.push_str(&format!("{} turns", self.turns));
        if let Some(c) = self.cost_usd {
            m.push_str(&format!(", ${c:.2}"));
        }
        if let (Some(model), Some(effort)) = (&self.model, &self.effort) {
            m.push_str(&format!(", by {model} at {effort} effort"));
        }
        m
    }

    /// The run an event tells of, if it tells of one.
    pub fn parse(kind: &str, message: &str) -> Option<Spend> {
        let (task, done) = [SpendTask::Rebase, SpendTask::Fix]
            .into_iter()
            .flat_map(|t| [(t, true), (t, false)])
            .find(|(t, done)| t.event_kind(*done) == kind)?;
        let (rest, model, effort) = match message
            .rsplit_once(", by ")
            .and_then(|(rest, by)| Some((rest, by.strip_suffix(" effort")?.rsplit_once(" at ")?)))
        {
            Some((rest, (model, effort))) => {
                (rest, Some(model.to_owned()), Some(effort.to_owned()))
            }
            None => (message, None, None),
        };
        let (rest, cost_usd) = match rest
            .rsplit_once(", $")
            .and_then(|(rest, c)| Some((rest, c.parse().ok()?)))
        {
            Some((rest, c)) => (rest, Some(c)),
            None => (rest, None),
        };
        let rest = rest
            .strip_suffix(" turns")
            .or_else(|| rest.strip_suffix(" turn"))?;
        let (files, turns) = rest.rsplit_once(" in ").unwrap_or(("", rest));
        Some(Spend {
            task,
            done,
            files: files.to_owned(),
            turns: turns.parse().ok()?,
            cost_usd,
            model,
            effort,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_reads_back() {
        let spend = Spend {
            task: SpendTask::Rebase,
            done: true,
            files: "src/a.rs, src/in b.rs".into(),
            turns: 46,
            cost_usd: Some(0.456),
            model: Some("claude-opus-5-5".into()),
            effort: Some("high".into()),
        };
        let message = spend.message();
        assert_eq!(
            message,
            "src/a.rs, src/in b.rs in 46 turns, $0.46, by claude-opus-5-5 at high effort"
        );
        let back = Spend::parse(spend.kind(), &message).unwrap();
        assert!(back.done);
        assert_eq!(back.files, spend.files);
        assert_eq!(back.cost_usd, Some(0.46));
        assert_eq!(back.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(back.effort.as_deref(), Some("high"));
    }

    #[test]
    fn older_messages_have_no_model() {
        let fix = Spend::parse("fix", "5 turns").unwrap();
        assert_eq!(
            (fix.task, fix.turns, fix.cost_usd),
            (SpendTask::Fix, 5, None)
        );
        assert_eq!(fix.model, None);
        let resolved = Spend::parse("resolved", "a.rs in 3 turns, $0.10").unwrap();
        assert_eq!(
            (resolved.files.as_str(), resolved.cost_usd),
            ("a.rs", Some(0.10))
        );
        assert_eq!(resolved.effort, None);
    }

    #[test]
    fn a_run_that_did_not_do_its_task_still_spent() {
        let fix = Spend::parse("unfixed", "7 turns, $0.12, by m at high effort").unwrap();
        assert_eq!(
            (fix.task, fix.done, fix.turns, fix.cost_usd),
            (SpendTask::Fix, false, 7, Some(0.12))
        );
        let rebase = Spend::parse("unresolved", "a.rs in 50 turns, by m at high effort").unwrap();
        assert_eq!(
            (rebase.task, rebase.done, rebase.cost_usd),
            (SpendTask::Rebase, false, None)
        );
    }

    #[test]
    fn other_events_are_no_run() {
        assert_eq!(Spend::parse("fixing", "5 turns"), None);
        assert_eq!(Spend::parse("fix", "gave up"), None);
    }
}
