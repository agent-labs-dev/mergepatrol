//! What one agent run spent, as its job event tells it: `resolved` for a rebase's conflict,
//! `fix` for failed checks. The job writes the message, the dashboard reads it back.

/// Why the agent ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SpendTask {
    /// It resolved a conflict in a rebase.
    Rebase,
    /// It fixed failed checks.
    Fix,
}

impl SpendTask {
    /// The job event that tells of the run.
    pub fn event_kind(self) -> &'static str {
        match self {
            SpendTask::Rebase => "resolved",
            SpendTask::Fix => "fix",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spend {
    pub task: SpendTask,
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
        let task = [SpendTask::Rebase, SpendTask::Fix]
            .into_iter()
            .find(|t| t.event_kind() == kind)?;
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
        let back = Spend::parse("resolved", &message).unwrap();
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
    fn other_events_are_no_run() {
        assert_eq!(Spend::parse("fixing", "5 turns"), None);
        assert_eq!(Spend::parse("fix", "gave up"), None);
    }
}
