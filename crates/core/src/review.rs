//! The agent's review of a queued PR, the queue's first gate: what it found, how sure it was,
//! and where each finding goes. A change it is sure enough of goes back to the PR's author, who
//! is often the agent that built it, to fix and push; the next head's review checks the fix. A
//! human is asked only about what the author cannot settle from the PR: a call on scope or
//! product, access, a change outside the repo, or a kind the workspace always asks about.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Hold, PrNumber, RepoName};

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

/// How sure the agent must be, in percent, to send a change of a kind back to the PR's author.
/// A lower bar sends back more. `Always` asks a human about every finding of the kind instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "BarText", into = "BarText")]
pub enum Bar {
    From(u8),
    Always,
}

impl Bar {
    /// The lowest bar: under it, a guess.
    pub const MIN: u8 = 50;

    /// Whether a change the agent is `confidence` percent sure of goes back to the author.
    pub fn sends(self, confidence: u8) -> bool {
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
    /// The gate also waits for every review thread on the PR to be resolved, whoever started
    /// it: its author resolves each, fixed or answered.
    #[serde(default = "yes")]
    pub require_resolved: bool,
}

fn yes() -> bool {
    true
}

/// On for every workspace: a review runs the agent, on the workspace's own key, and runs only
/// once it has one. What hurts most when it is wrong goes back to the author on less certainty;
/// migrations always ask a human.
impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            bars: Kind::ALL.map(|k| (k, default_bar(k))).into(),
            require_resolved: true,
        }
    }
}

fn default_bar(kind: Kind) -> Bar {
    match kind {
        Kind::Security => Bar::From(60),
        Kind::Migrations => Bar::Always,
        Kind::Dependencies | Kind::PublicApi => Bar::From(70),
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
            require_resolved: repo.require_resolved.unwrap_or(self.require_resolved),
        }
    }

    /// Where a finding goes. A bug the PR did not bring holds nothing: it is a note, whatever its
    /// kind. A human only for a kind that always asks, or a change the author
    /// cannot make from the PR; a change the agent is sure enough of, that shows what it is
    /// about, goes back to the author; the rest it clears, a change under the bar or one that
    /// shows nothing as a note.
    pub fn route(&self, finding: &Finding) -> Route {
        let bar = self.bar(finding.kind);
        match finding.lean {
            Lean::PreExisting => Route::Cleared,
            _ if bar == Bar::Always => Route::Human,
            Lean::Fine => Route::Cleared,
            Lean::Change if finding.needs_human => Route::Human,
            Lean::Change if !finding.shows_the_change() => Route::Cleared,
            Lean::Change if bar.sends(finding.confidence) => Route::Author,
            Lean::Change => Route::Cleared,
        }
    }
}

/// Where a finding goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Cleared,
    /// Back to the PR's author, to change and push.
    Author,
    Human,
}

/// What a repo's config file sets of the review, over the workspace's settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bars: BTreeMap<Kind, Bar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_resolved: Option<bool>,
}

impl ReviewOverrides {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.bars.is_empty() && self.require_resolved.is_none()
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
    /// A bug in code the PR did not bring: not the author's to fix here, so a note, to file as
    /// an issue.
    PreExisting,
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
    /// What shows the change is needed: a command that fails on the PR's head. A `change`
    /// carries one that MergeLoop ran and saw fail, or a security one a panel's votes, or it is
    /// only a note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// The change needs a call the author cannot make from the PR: scope, product or design,
    /// access or credentials, or a change outside the repo.
    #[serde(default)]
    pub needs_human: bool,
    /// What a panel of fresh agents made of it, each given only the finding and the code: for a
    /// change that shows nothing MergeLoop ran, whether enough agree it holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified: Option<Verification>,
    /// The lines it is about, from the PR's diff.
    #[serde(default)]
    pub code: Vec<CodeLine>,
    /// Other code it read to get here.
    #[serde(default)]
    pub related: Vec<Related>,
    /// The other places the same bug is, past `file` and `line`: a fix settles every one.
    #[serde(default)]
    pub also_at: Vec<Place>,
}

/// Another place a finding's bug is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub file: String,
    #[serde(default)]
    pub line: Option<u32>,
    /// Its lines, from the PR's diff.
    #[serde(default)]
    pub code: Vec<CodeLine>,
}

/// The longest new-issue link a finding gives: GitHub drops much longer ones, and a review's
/// summary holds one for each of up to 25 findings.
pub const ISSUE_URL_CHARS: usize = 1_500;

impl Finding {
    /// A link that opens a new issue on `repo` for a bug the PR did not bring, filled in from
    /// the finding: `None` for any other finding. It fits `ISSUE_URL_CHARS`, whatever the agent
    /// wrote.
    pub fn issue_url(&self, repo: &RepoName, pr: PrNumber) -> Option<String> {
        if self.lean != Lean::PreExisting {
            return None;
        }
        let link = |title: &str, body: &str| {
            format!(
                "https://github.com/{repo}/issues/new?title={}&body={}",
                encode(title),
                encode(body)
            )
        };
        let found = format!("Found by MergeLoop's review of #{pr}, which did not bring it.");
        let at = match self.places().as_slice() {
            [] => String::new(),
            places => format!("`{}`\n\n", places.join("`, `")),
        };
        let full = link(&self.title, &format!("{at}{}\n\n{found}", self.finding));
        if full.len() <= ISSUE_URL_CHARS {
            return Some(full);
        }
        // 80 characters of 4 bytes, each byte `%XX`, leave room for the rest.
        let title: String = self.title.chars().take(80).collect();
        Some(link(&title, &found))
    }

    /// Every place its bug is, as a file and maybe a line: its own, then the others it names.
    pub fn locations(&self) -> impl Iterator<Item = (&str, Option<u32>)> {
        let own = self.file.as_deref().map(|file| (file, self.line));
        let others = self.also_at.iter().map(|p| (p.file.as_str(), p.line));
        own.into_iter().chain(others)
    }

    /// Every place its bug is, as `file:line` or `file`: its own, then the others it names.
    pub fn places(&self) -> Vec<String> {
        self.locations()
            .map(|(file, line)| at(file, line))
            .collect()
    }

    /// Whether it shows the change is needed: MergeLoop ran its evidence on the PR's head and it
    /// failed. A `security` finding may show it by the code alone instead, when enough of a panel
    /// of fresh agents agree that it holds.
    pub fn shows_the_change(&self) -> bool {
        self.ran_failing()
            || (self.kind == Kind::Security && self.verified.as_ref().is_some_and(|v| v.upheld()))
    }

    /// Whether its route waits on a panel's votes: a `security` change for the author to make,
    /// with no evidence MergeLoop saw fail.
    pub fn wants_votes(&self) -> bool {
        self.kind == Kind::Security
            && self.lean == Lean::Change
            && !self.needs_human
            && !self.ran_failing()
    }

    fn ran_failing(&self) -> bool {
        self.evidence.as_ref().is_some_and(|e| e.failed)
    }
}

/// A panel of fresh agents that each check one thing on their own: how many sit, and how many
/// must agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Panel {
    pub seats: u8,
    pub needed: u8,
}

impl Panel {
    /// The panel that checks a security change the code alone shows: three, two of whom must
    /// agree it holds.
    pub const SECURITY: Panel = Panel {
        seats: 3,
        needed: 2,
    };

    /// What the panel decided from these votes: only the first `seats` count.
    pub fn verdict(self, mut votes: Vec<Vote>) -> Verification {
        votes.truncate(self.seats.into());
        Verification {
            needed: self.needed,
            votes,
        }
    }
}

/// What a panel decided of a finding, with each vote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verification {
    /// How many had to agree it holds.
    pub needed: u8,
    pub votes: Vec<Vote>,
}

impl Verification {
    /// How many said it holds.
    pub fn holds(&self) -> usize {
        let holds = self.votes.iter().filter(|v| v.verdict == Verdict::Holds);
        holds.count()
    }

    /// Enough agreed it holds.
    pub fn upheld(&self) -> bool {
        self.holds() >= usize::from(self.needed)
    }
}

/// One agent's vote, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vote {
    pub verdict: Verdict,
    #[serde(default)]
    pub why: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Holds,
    DoesNotHold,
    /// The agent's run broke, or it named no verdict.
    Abstained,
}

/// A place as `file:line`, or `file` without a line.
fn at(file: &str, line: Option<u32>) -> String {
    match line {
        Some(line) => format!("{file}:{line}"),
        None => file.to_owned(),
    }
}

impl std::fmt::Display for Place {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&at(&self.file, self.line))
    }
}

/// `text` as a URL's query value.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A command that shows a change is needed by failing, with any test the agent wrote for it. The
/// agent names it; only MergeLoop's own run of it on the PR's head counts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// What to run from the repo's root, e.g. `pnpm vitest run src/retry.test.ts`.
    pub command: String,
    /// The files the agent wrote or changed to show it: a test, most often.
    #[serde(default)]
    pub files: Vec<String>,
    /// Those files' changes on the PR's head, as MergeLoop took them after the review. The fix
    /// starts from them, and must leave them as they are.
    #[serde(default)]
    pub patch: String,
    /// The end of what the command printed when MergeLoop ran it.
    #[serde(default)]
    pub output: String,
    /// MergeLoop ran it on the PR's head, with `patch`, and it failed.
    #[serde(default)]
    pub failed: bool,
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
    /// The agent sent findings back to the author, or a human asked for changes: it waits for
    /// a push.
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

    /// What the queue's first gate holds a PR for while its review is here, or `None` once the
    /// review lets it on.
    pub fn hold(self) -> Option<Hold> {
        match self {
            ReviewStatus::Approved => None,
            ReviewStatus::Pending | ReviewStatus::Running | ReviewStatus::Stale => {
                Some(Hold::Reviewing)
            }
            ReviewStatus::Asks => Some(Hold::ReviewAsks),
            ReviewStatus::Changes => Some(Hold::ReviewChanges),
        }
    }
}

/// Where a PR's review is on the dashboard, to answer it: the PR in its lane into `trunk`.
pub fn review_url(public_url: &str, repo: &RepoName, trunk: &str, number: PrNumber) -> String {
    format!(
        "{}/{}/queue?lane={repo}:{trunk}&pr={number}&answer=true",
        public_url.trim_end_matches('/'),
        repo.owner,
    )
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

/// A finding the last review sent back to the author, as the next review sees it: with what the
/// author said in its thread, and whether they resolved it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentBack {
    pub finding: Finding,
    /// The author's last reply in its thread. Empty when there is none.
    #[serde(default)]
    pub reply: String,
    #[serde(default)]
    pub resolved: bool,
}

/// A finding as the review keeps it: whether the agent asks a human about it or sent it back
/// to the author, and the bar it was held to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Judged {
    #[serde(flatten)]
    pub finding: Finding,
    pub asked: bool,
    /// Sent back to the author, and not yet withdrawn: the PR waits for a push that fixes it.
    #[serde(default)]
    pub sent_back: bool,
    /// The agent withdrew it after the author's reply.
    #[serde(default)]
    pub withdrawn: bool,
    /// It has a review thread on the PR, which its author resolves once it is fixed or answered.
    #[serde(default)]
    pub threaded: bool,
    pub bar: Bar,
}

impl ReviewSettings {
    /// Each finding, with where it goes.
    pub fn judge(&self, findings: Vec<Finding>) -> Vec<Judged> {
        findings
            .into_iter()
            .map(|finding| {
                let route = self.route(&finding);
                Judged {
                    asked: route == Route::Human,
                    sent_back: route == Route::Author,
                    withdrawn: false,
                    threaded: false,
                    bar: self.bar(finding.kind),
                    finding,
                }
            })
            .collect()
    }
}

/// How a review ends: a human is asked when anything goes to one or the agent could not review;
/// else it waits for the author's push while anything is sent back; else it is approved.
pub fn review_status(findings: &[Judged], failed: bool) -> ReviewStatus {
    if failed || findings.iter().any(|f| f.asked) {
        ReviewStatus::Asks
    } else if findings.iter().any(|f| f.sent_back) {
        ReviewStatus::Changes
    } else {
        ReviewStatus::Approved
    }
}

/// A question to the agent about a finding, and its answer once it has one: a human's, or the
/// author's reply to a finding sent back to them.
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
            evidence: Some(Evidence {
                command: "cargo test".into(),
                failed: true,
                ..Evidence::default()
            }),
            needs_human: false,
            verified: None,
            code: Vec::new(),
            related: Vec::new(),
            also_at: Vec::new(),
        }
    }

    #[test]
    fn a_finding_names_every_place_its_bug_is() {
        let f: Finding = serde_json::from_value(serde_json::json!({
            "kind": "other", "title": "t", "file": "a.rs", "line": 3,
            "confidence": 90, "lean": "change", "finding": "f",
            "also_at": [{ "file": "b.rs", "line": 9 }, { "file": "c.rs" }],
        }))
        .unwrap();
        assert_eq!(f.places(), ["a.rs:3", "b.rs:9", "c.rs"]);
        // A review stored before `also_at` reads as one place.
        let old: Finding = serde_json::from_value(serde_json::json!({
            "kind": "other", "title": "t", "file": "a.rs",
            "confidence": 90, "lean": "change", "finding": "f",
        }))
        .unwrap();
        assert_eq!(old.places(), ["a.rs"]);
        // One about the whole PR is at no place.
        assert!(finding(Kind::Other, 90, Lean::Change).places().is_empty());
    }

    #[test]
    fn changes_go_back_to_the_author_and_only_calls_they_cannot_make_go_to_a_human() {
        let s = ReviewSettings::default();
        assert!(s.enabled);
        assert_eq!(
            s.route(&finding(Kind::Other, 60, Lean::Fine)),
            Route::Cleared
        );
        assert_eq!(
            s.route(&finding(Kind::Other, 80, Lean::Change)),
            Route::Author
        );
        // Under the bar, a change is a note.
        assert_eq!(
            s.route(&finding(Kind::Other, 79, Lean::Change)),
            Route::Cleared
        );
        // A change that shows no test, repro or run result is a note, however sure.
        let mut bare = finding(Kind::Other, 100, Lean::Change);
        bare.evidence = None;
        assert_eq!(s.route(&bare), Route::Cleared);
        // What hurts more goes back on less certainty.
        assert_eq!(
            s.route(&finding(Kind::Security, 60, Lean::Change)),
            Route::Author
        );
        let mut call = finding(Kind::Other, 100, Lean::Change);
        call.needs_human = true;
        assert_eq!(s.route(&call), Route::Human);
        // However sure, and however it leans: migrations always ask.
        assert_eq!(
            s.route(&finding(Kind::Migrations, 100, Lean::Fine)),
            Route::Human
        );
    }

    #[test]
    fn a_pre_existing_bug_is_a_note_that_never_holds_the_pr() {
        let s = ReviewSettings::default();
        // However sure, of whatever kind, and whatever else it says: the PR did not bring it.
        for kind in Kind::ALL {
            let mut f = finding(kind, 100, Lean::PreExisting);
            f.needs_human = true;
            assert_eq!(s.route(&f), Route::Cleared, "{kind:?}");
            let judged = s.judge(vec![f]);
            assert_eq!(
                review_status(&judged, false),
                ReviewStatus::Approved,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_pre_existing_bug_links_to_an_issue_filled_in_from_it() {
        let repo = RepoName::parse("o/r").unwrap();
        let mut f = finding(Kind::Other, 90, Lean::PreExisting);
        f.title = "Retry & loop".into();
        f.file = Some("src/a.rs".into());
        f.line = Some(7);
        f.finding = "It spins.".into();
        assert_eq!(
            f.issue_url(&repo, 12).as_deref(),
            Some(
                "https://github.com/o/r/issues/new?title=Retry%20%26%20loop&body=%60src%2Fa.rs%3A7%60%0A%0AIt%20spins.%0A%0AFound%20by%20MergeLoop%27s%20review%20of%20%2312%2C%20which%20did%20not%20bring%20it."
            )
        );
        // Only a pre-existing one: the rest are the PR's to settle.
        assert_eq!(
            finding(Kind::Other, 90, Lean::Change).issue_url(&repo, 12),
            None
        );
        // However long, and in whatever script, the agent wrote.
        f.title = "🐛".repeat(10_000);
        f.finding = "é".repeat(10_000);
        let long = f.issue_url(&repo, 12).unwrap();
        assert!(long.len() <= ISSUE_URL_CHARS, "{}", long.len());
        assert!(long.contains("%2312"));
        // A bug that recurs is filed once, with every place it is.
        let mut many = finding(Kind::Other, 90, Lean::PreExisting);
        many.file = Some("a.rs".into());
        many.line = Some(7);
        many.also_at = vec![Place {
            file: "b.rs".into(),
            line: Some(2),
            code: Vec::new(),
        }];
        let url = many.issue_url(&repo, 12).unwrap();
        assert!(
            url.contains("body=%60a.rs%3A7%60%2C%20%60b.rs%3A2%60%0A%0A"),
            "{url}"
        );
    }

    #[test]
    fn a_change_needs_a_command_mergeloop_saw_fail_unless_it_is_a_security_one() {
        let s = ReviewSettings::default();
        for kind in [Kind::Other, Kind::Dependencies, Kind::PublicApi] {
            let mut f = finding(kind, 100, Lean::Change);
            f.evidence = None;
            assert_eq!(s.route(&f), Route::Cleared, "{kind:?}");
            // What the agent says it ran is not evidence: only MergeLoop's run is.
            f.evidence = Some(Evidence {
                command: "cargo test -p core retry".into(),
                output: "test retry ... FAILED".into(),
                ..Evidence::default()
            });
            assert_eq!(s.route(&f), Route::Cleared, "{kind:?}");
            f.evidence.as_mut().unwrap().failed = true;
            assert_eq!(s.route(&f), Route::Author, "{kind:?}");
        }
        // A human is still asked about a change the author cannot make, evidence or not.
        let mut call = finding(Kind::PublicApi, 100, Lean::Change);
        call.evidence = None;
        call.needs_human = true;
        assert_eq!(s.route(&call), Route::Human);
    }

    fn votes(verdicts: &[Verdict]) -> Vec<Vote> {
        verdicts
            .iter()
            .map(|&verdict| Vote {
                verdict,
                why: "w".into(),
            })
            .collect()
    }

    #[test]
    fn a_security_change_with_no_evidence_goes_back_only_if_enough_fresh_agents_agree() {
        use Verdict::*;
        let s = ReviewSettings::default();
        let mut hole = finding(Kind::Security, 100, Lean::Change);
        hole.evidence = None;
        // One agent's word is a note.
        assert!(hole.wants_votes());
        assert_eq!(s.route(&hole), Route::Cleared);
        let panel = Panel::SECURITY;
        for (cast, route) in [
            (vec![Holds, Holds, DoesNotHold], Route::Author),
            (vec![Holds, DoesNotHold, Abstained], Route::Cleared),
            (vec![Holds, Abstained, Abstained], Route::Cleared),
            (vec![Holds, Holds, Holds], Route::Author),
        ] {
            hole.verified = Some(panel.verdict(votes(&cast)));
            assert_eq!(s.route(&hole), route, "{cast:?}");
        }
        // Only the panel's seats vote: more votes posted do not outvote it.
        let stuffed = panel.verdict(votes(&[DoesNotHold, DoesNotHold, Holds, Holds, Holds]));
        assert_eq!(stuffed.votes.len(), 3);
        assert!(!stuffed.upheld());
        // Evidence MergeLoop saw fail shows it without a vote, as for any kind.
        let shown = finding(Kind::Security, 100, Lean::Change);
        assert!(!shown.wants_votes());
        assert_eq!(s.route(&shown), Route::Author);
        // Votes do not stand in for evidence on other kinds.
        let mut other = finding(Kind::Other, 100, Lean::Change);
        other.evidence = None;
        other.verified = Some(panel.verdict(votes(&[Holds, Holds, Holds])));
        assert!(!other.wants_votes());
        assert_eq!(s.route(&other), Route::Cleared);
        // Nothing to vote on where the votes would not change the route.
        let mut call = hole.clone();
        call.needs_human = true;
        let mut fine = hole.clone();
        fine.lean = Lean::Fine;
        assert!(!call.wants_votes() && !fine.wants_votes());
    }

    #[test]
    fn a_review_waits_for_a_human_then_the_author_then_lets_the_pr_on() {
        let s = ReviewSettings::default();
        let fine = s.judge(vec![finding(Kind::Other, 90, Lean::Fine)]);
        assert_eq!(review_status(&fine, false), ReviewStatus::Approved);
        assert_eq!(review_status(&fine, true), ReviewStatus::Asks);
        let back = s.judge(vec![
            finding(Kind::Other, 90, Lean::Fine),
            finding(Kind::Other, 90, Lean::Change),
        ]);
        assert_eq!(review_status(&back, false), ReviewStatus::Changes);
        let mut call = finding(Kind::Other, 90, Lean::Change);
        call.needs_human = true;
        let both = s.judge(vec![finding(Kind::Other, 90, Lean::Change), call]);
        assert_eq!(review_status(&both, false), ReviewStatus::Asks);
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
            require_resolved: None,
        };
        let s = ws.with(&repo);
        assert!(s.enabled);
        assert_eq!(s.bar(Kind::Security), Bar::Always);
        assert_eq!(s.bar(Kind::Other), Bar::From(65));
        assert_eq!(s.bar(Kind::Dependencies), Bar::From(70));
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
