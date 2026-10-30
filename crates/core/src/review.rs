//! The agent's review of a queued PR, the queue's first gate: what it found, how sure it was,
//! and which findings it asks a human about. A finding it leans to accept, and is sure enough
//! of, it clears itself. Any other finding goes to a human: the agent approves alone, but never
//! sends a PR back alone.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// What a finding is about. Some kinds hurt more when they are wrong, so each has its own bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Auth, secrets, crypto, input that crosses a trust boundary.
    Security,
    /// Schema and data migrations.
    Migrations,
    /// Lockfiles, new or bumped packages.
    Dependencies,
    /// Routes, exported types, CLI flags, config keys: what others build on.
    PublicApi,
    Other,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Security,
        Kind::Migrations,
        Kind::Dependencies,
        Kind::PublicApi,
        Kind::Other,
    ];

    /// As the config file and the API name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Security => "security",
            Kind::Migrations => "migrations",
            Kind::Dependencies => "dependencies",
            Kind::PublicApi => "public_api",
            Kind::Other => "other",
        }
    }
}

/// How sure the agent must be, in percent, to clear a finding of a kind alone. `Always` asks a
/// human however sure it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "BarText", into = "BarText")]
pub enum Bar {
    From(u8),
    Always,
}

impl Bar {
    /// The lowest bar: under it, a guess.
    pub const MIN: u8 = 50;

    /// Whether a finding the agent is `confidence` percent sure of clears.
    pub fn clears(self, confidence: u8) -> bool {
        match self {
            Bar::From(bar) => confidence >= bar,
            Bar::Always => false,
        }
    }
}

/// A bar as written: a percent, or `always_ask`.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum BarText {
    Percent(u8),
    Word(String),
}

const ALWAYS_ASK: &str = "always_ask";

impl TryFrom<BarText> for Bar {
    type Error = String;

    fn try_from(text: BarText) -> Result<Self, String> {
        match text {
            BarText::Percent(p) if (Bar::MIN..=100).contains(&p) => Ok(Bar::From(p)),
            BarText::Percent(p) => Err(format!("{p} is not a percent from {} to 100", Bar::MIN)),
            BarText::Word(w) if w == ALWAYS_ASK => Ok(Bar::Always),
            BarText::Word(w) => Err(format!("{w:?} is not a percent or `{ALWAYS_ASK}`")),
        }
    }
}

impl From<Bar> for BarText {
    fn from(bar: Bar) -> Self {
        match bar {
            Bar::From(p) => BarText::Percent(p),
            Bar::Always => BarText::Word(ALWAYS_ASK.into()),
        }
    }
}

/// A workspace's review settings: whether its repos' queues review, and each kind's bar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSettings {
    pub enabled: bool,
    /// Every kind's bar.
    pub bars: BTreeMap<Kind, Bar>,
}

/// Off until a workspace turns it on: a review runs the agent, on the workspace's own key.
/// The bars ask about what hurts most when it is wrong, and let the rest through from 80%.
impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bars: Kind::ALL.map(|k| (k, default_bar(k))).into(),
        }
    }
}

fn default_bar(kind: Kind) -> Bar {
    match kind {
        Kind::Security => Bar::From(90),
        Kind::Migrations => Bar::Always,
        Kind::Dependencies | Kind::PublicApi => Bar::From(85),
        Kind::Other => Bar::From(80),
    }
}

impl ReviewSettings {
    /// A kind's bar. A bar missing from stored settings is the default's.
    pub fn bar(&self, kind: Kind) -> Bar {
        self.bars.get(&kind).copied().unwrap_or(default_bar(kind))
    }

    /// The settings a repo runs with: the workspace's, under what its config file sets.
    pub fn with(&self, repo: &ReviewOverrides) -> ReviewSettings {
        let mut bars: BTreeMap<Kind, Bar> = Kind::ALL.map(|k| (k, self.bar(k))).into();
        bars.extend(&repo.bars);
        ReviewSettings {
            enabled: repo.enabled.unwrap_or(self.enabled),
            bars,
        }
    }

    /// Whether the agent asks a human about a finding: it leans to a change, or it is not sure
    /// enough to clear it.
    pub fn asks(&self, finding: &Finding) -> bool {
        finding.lean == Lean::Change || !self.bar(finding.kind).clears(finding.confidence)
    }
}

/// What a repo's config file sets of the review, over the workspace's settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bars: BTreeMap<Kind, Bar>,
}

impl ReviewOverrides {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.bars.is_empty()
    }
}

/// What the agent thinks should happen to what it found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lean {
    /// Fine as it is.
    Fine,
    /// The author should change it.
    Change,
}

/// One thing the agent looked at, with what it decided and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// `f1`, `f2`, … in the review's order.
    #[serde(default)]
    pub id: String,
    pub kind: Kind,
    pub title: String,
    /// The file and the line, on the PR's side, it is about. A finding about the whole PR has
    /// neither.
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    /// How sure the agent is that `lean` is right, in percent.
    pub confidence: u8,
    pub lean: Lean,
    /// What it saw.
    pub finding: String,
    /// Why it would not decide alone: what it could not tell from the code. Empty when it was
    /// sure.
    #[serde(default)]
    pub unsure: String,
    /// What it checked already, so a human need not.
    #[serde(default)]
    pub checked: Vec<String>,
    /// What would have let it decide alone.
    #[serde(default)]
    pub would_clear: String,
    /// The change it would ask the author for.
    #[serde(default)]
    pub suggestion: String,
    /// The lines it is about, from the PR's diff.
    #[serde(default)]
    pub code: Vec<CodeLine>,
    /// Other code it read to get here.
    #[serde(default)]
    pub related: Vec<Related>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Related {
    pub file: String,
    #[serde(default)]
    pub line: Option<u32>,
    pub why: String,
    #[serde(default)]
    pub code: Vec<CodeLine>,
}

/// A line of code as a review shows it: its number on its side, and whether the PR added it,
/// removed it, or left it as context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeLine {
    pub number: Option<u32>,
    pub kind: LineKind,
    pub text: String,
    /// One of the lines the finding is about.
    #[serde(default)]
    pub hot: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// Where a review is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    /// Waiting for the agent.
    Pending,
    Running,
    /// Waiting for a human's answers.
    Asks,
    Approved,
    /// A human asked for changes: it waits for a push.
    Changes,
    /// The PR moved on before the review finished: a newer head has its own.
    Stale,
}

impl ReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewStatus::Pending => "pending",
            ReviewStatus::Running => "running",
            ReviewStatus::Asks => "asks",
            ReviewStatus::Approved => "approved",
            ReviewStatus::Changes => "changes",
            ReviewStatus::Stale => "stale",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            ReviewStatus::Pending,
            ReviewStatus::Running,
            ReviewStatus::Asks,
            ReviewStatus::Approved,
            ReviewStatus::Changes,
            ReviewStatus::Stale,
        ]
        .into_iter()
        .find(|s2| s2.as_str() == s)
    }
}

/// A human's answer about one finding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum Answer {
    Fine,
    Change { note: String },
}

/// A finding a human answered on an earlier head of the PR. The next review does not ask about
/// it again unless its lines changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answered {
    pub finding: Finding,
    pub answer: Answer,
    pub by: String,
}

/// A finding as the review keeps it: whether the agent asks a human about it, and the bar it
/// was held to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Judged {
    #[serde(flatten)]
    pub finding: Finding,
    pub asked: bool,
    pub bar: Bar,
}

impl ReviewSettings {
    /// Each finding, with whether the agent asks about it.
    pub fn judge(&self, findings: Vec<Finding>) -> Vec<Judged> {
        findings
            .into_iter()
            .map(|finding| Judged {
                asked: self.asks(&finding),
                bar: self.bar(finding.kind),
                finding,
            })
            .collect()
    }
}

/// A human's question to the agent about a finding, and its answer once it has one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// `q1`, `q2`, … in the order asked.
    pub id: String,
    /// The finding's id.
    pub finding: String,
    pub question: String,
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Why the agent could not answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Question {
    /// Waiting for the agent.
    pub fn pending(&self) -> bool {
        self.answer.is_none() && self.error.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(kind: Kind, confidence: u8, lean: Lean) -> Finding {
        Finding {
            id: "f1".into(),
            kind,
            title: "t".into(),
            file: None,
            line: None,
            confidence,
            lean,
            finding: String::new(),
            unsure: String::new(),
            checked: Vec::new(),
            would_clear: String::new(),
            suggestion: String::new(),
            code: Vec::new(),
            related: Vec::new(),
        }
    }

    #[test]
    fn the_agent_clears_alone_only_what_it_accepts_and_is_sure_enough_of() {
        let s = ReviewSettings::default();
        assert!(!s.asks(&finding(Kind::Other, 80, Lean::Fine)));
        assert!(s.asks(&finding(Kind::Other, 79, Lean::Fine)));
        assert!(s.asks(&finding(Kind::Security, 89, Lean::Fine)));
        assert!(!s.asks(&finding(Kind::Security, 90, Lean::Fine)));
        // However sure: migrations always ask, and so does any change.
        assert!(s.asks(&finding(Kind::Migrations, 100, Lean::Fine)));
        assert!(s.asks(&finding(Kind::Other, 100, Lean::Change)));
    }

    #[test]
    fn a_repo_overrides_what_it_sets_and_keeps_the_rest() {
        let ws = ReviewSettings {
            enabled: true,
            ..ReviewSettings::default()
        };
        let repo = ReviewOverrides {
            enabled: None,
            bars: BTreeMap::from([(Kind::Security, Bar::Always), (Kind::Other, Bar::From(65))]),
        };
        let s = ws.with(&repo);
        assert!(s.enabled);
        assert_eq!(s.bar(Kind::Security), Bar::Always);
        assert_eq!(s.bar(Kind::Other), Bar::From(65));
        assert_eq!(s.bar(Kind::Dependencies), Bar::From(85));
        let off = ws.with(&ReviewOverrides {
            enabled: Some(false),
            ..Default::default()
        });
        assert!(!off.enabled);
    }

    #[test]
    fn bars_read_as_percents_or_always_ask() {
        let o: ReviewOverrides =
            serde_yaml::from_str("bars: { security: always_ask, other: 70 }").unwrap();
        assert_eq!(o.bars[&Kind::Security], Bar::Always);
        assert_eq!(o.bars[&Kind::Other], Bar::From(70));
        assert_eq!(
            serde_yaml::to_string(&o).unwrap(),
            "bars:\n  security: always_ask\n  other: 70\n"
        );
        for bad in [
            "bars: { other: 40 }",
            "bars: { other: 101 }",
            "bars: { other: never }",
            "bars: { styling: 80 }",
        ] {
            assert!(
                serde_yaml::from_str::<ReviewOverrides>(bad).is_err(),
                "{bad}"
            );
        }
    }
}
