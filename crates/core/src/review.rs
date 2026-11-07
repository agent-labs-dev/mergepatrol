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

/// How hard a review looks, which its spend follows. `Deep` is for code where one missed bug costs
/// more than the extra review.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    /// One reviewer, who reports only what could be wrong.
    Quick,
    /// One reviewer, who also says what it checked and found sound.
    #[default]
    Standard,
    /// Several reviewers at once, each fresh, whose findings merge; a panel then votes on every
    /// change nothing MergeLoop ran shows, of any kind.
    Deep,
}

impl Depth {
    pub const ALL: [Depth; 3] = [Depth::Quick, Depth::Standard, Depth::Deep];

    /// As the config file and the API name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Depth::Quick => "quick",
            Depth::Standard => "standard",
            Depth::Deep => "deep",
        }
    }

    /// How many reviewers look, each fresh and on its own.
    pub fn reviewers(self) -> u8 {
        match self {
            Depth::Quick | Depth::Standard => 1,
            Depth::Deep => 3,
        }
    }

    /// Whether a panel's votes may show a change of `kind` that no command MergeLoop ran showed.
    pub fn votes_on(self, kind: Kind) -> bool {
        self == Depth::Deep || kind == Kind::Security
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
    #[serde(default)]
    pub depth: Depth,
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
            depth: Depth::default(),
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
            depth: repo.depth.unwrap_or(self.depth),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<Depth>,
}

impl ReviewOverrides {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
            && self.bars.is_empty()
            && self.require_resolved.is_none()
            && self.depth.is_none()
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
    /// On a `fine` finding that reports again one the last review held the PR for: how it was
    /// settled.
    /// Anything else the agent writes there reads as none: a stray value costs an outcome, never
    /// the review.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "settled_or_none"
    )]
    pub settled: Option<Settled>,
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
    /// Whether it is about `title`: as a later review raises the same finding again, however it
    /// is cased or spaced.
    pub fn titled(&self, title: &str) -> bool {
        self.title.trim().eq_ignore_ascii_case(title.trim())
    }

    /// Whether `other` raises the same finding.
    pub fn same_as(&self, other: &Finding) -> bool {
        self.titled(&other.title)
    }

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
    /// failed, or enough of a panel of fresh agents agree that it holds. Only a finding that
    /// `wants_votes` keeps a panel's votes: where they come in, the rest are dropped.
    pub fn shows_the_change(&self) -> bool {
        self.ran_failing() || self.verified.as_ref().is_some_and(|v| v.upheld())
    }

    /// Whether its route waits on a panel's votes: a change for the author to make, with no
    /// evidence MergeLoop saw fail, of a kind `depth` votes on.
    pub fn wants_votes(&self, depth: Depth) -> bool {
        depth.votes_on(self.kind)
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
    /// The panel that checks a change the code alone shows: three, two of whom must agree it
    /// holds.
    pub const FINDING: Panel = Panel {
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

/// How far apart two reviewers' lines may be and still be about the same issue.
const SAME_PLACE_LINES: u32 = 3;

/// Several reviewers' findings, one for each issue, in the order first found: two of different
/// reviewers are of the same issue when they are of the same kind and about the same place, the
/// same file a few lines apart at any place either names, or, with no line, the same title. Of
/// those, the one that says most stands for both, and names every place either did: a change
/// over a note, evidence over none, then the surer. A reviewer's own findings are as many issues
/// as it says.
pub fn merge_reviews(reviews: Vec<Vec<Finding>>) -> Vec<Finding> {
    // Each issue: the finding that stands for it, and the reviewers who found it.
    let mut issues: Vec<(Finding, Vec<usize>)> = Vec::new();
    for (by, review) in reviews.into_iter().enumerate() {
        for f in review {
            let same = issues
                .iter_mut()
                .find(|(standing, found)| !found.contains(&by) && same_issue(standing, &f));
            match same {
                Some((standing, found)) => {
                    found.push(by);
                    match says_more(&f, standing) {
                        true => {
                            let other = std::mem::replace(standing, f);
                            take_places(standing, &other);
                        }
                        false => take_places(standing, &f),
                    }
                }
                None => issues.push((f, vec![by])),
            }
        }
    }
    issues.into_iter().map(|(f, _)| f).collect()
}

fn same_place((a, x): (&str, Option<u32>), (b, y): (&str, Option<u32>)) -> bool {
    a == b && matches!((x, y), (Some(x), Some(y)) if x.abs_diff(y) <= SAME_PLACE_LINES)
}

/// Adds to `keep` the places of `other` it does not already name.
fn take_places(keep: &mut Finding, other: &Finding) {
    let new: Vec<Place> = other
        .locations()
        .filter(|&p| !keep.locations().any(|q| same_place(p, q) || p == q))
        .map(|(file, line)| Place {
            file: file.to_owned(),
            line,
            code: Vec::new(),
        })
        .collect();
    keep.also_at.extend(new);
}

fn same_issue(a: &Finding, b: &Finding) -> bool {
    let near = || {
        a.locations()
            .any(|p| b.locations().any(|q| same_place(p, q)))
    };
    let titled = || a.file == b.file && title_words(&a.title) == title_words(&b.title);
    a.kind == b.kind
        && match (a.line, b.line) {
            (Some(_), Some(_)) => near(),
            _ => near() || titled(),
        }
}

fn title_words(title: &str) -> Vec<String> {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whether `a` says more of an issue than `b`.
fn says_more(a: &Finding, b: &Finding) -> bool {
    let weight = |f: &Finding| (weight(f.lean), f.evidence.is_some(), f.confidence);
    weight(a) > weight(b)
}

/// How much a lean asks of anyone: a change, then a bug to file, then a note.
fn weight(lean: Lean) -> u8 {
    match lean {
        Lean::Change => 2,
        Lean::PreExisting => 1,
        Lean::Fine => 0,
    }
}

/// At most `max` findings, in the order found: what a cap drops is the notes first, then the bugs
/// to file, never a change while a note is left.
pub fn most_first(findings: Vec<Finding>, max: usize) -> Vec<Finding> {
    let mut order: Vec<usize> = (0..findings.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(weight(findings[i].lean)));
    order.truncate(max);
    order.sort_unstable();
    let mut findings: Vec<Option<Finding>> = findings.into_iter().map(Some).collect();
    order.iter().filter_map(|&i| findings[i].take()).collect()
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
    /// What became of it, once that is known: of a change sent back or asked of a human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

/// What became of a finding that asked for a change, as the blog post on Claude Code's review
/// sorts a comment by how its author took it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The code changed to fix it.
    Fixed,
    /// The finding was wrong, or did not apply: the author's reply held up in the code.
    Wrong,
    /// The code does what its author meant it to, and that holds up.
    Intended,
    /// Let go without a fix or a reply that held up: the author resolved its thread with no
    /// push, or a human called it fine.
    Dismissed,
}

/// How the next review says a finding the last held the PR for was settled. Only a person
/// dismisses one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Settled {
    Fixed,
    Wrong,
    Intended,
}

fn settled_or_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Settled>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Written {
        Text(String),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Written::deserialize(d)? {
        Written::Text(t) if t == "fixed" => Some(Settled::Fixed),
        Written::Text(t) if t == "wrong" => Some(Settled::Wrong),
        Written::Text(t) if t == "intended" => Some(Settled::Intended),
        Written::Text(_) | Written::Other(_) => None,
    })
}

impl From<Settled> for Outcome {
    fn from(s: Settled) -> Self {
        match s {
            Settled::Fixed => Outcome::Fixed,
            Settled::Wrong => Outcome::Wrong,
            Settled::Intended => Outcome::Intended,
        }
    }
}

impl Judged {
    /// Records what became of it. The first word on it stands.
    pub fn settle(&mut self, outcome: Outcome) {
        self.outcome.get_or_insert(outcome);
    }

    /// Whether it held the PR for a change, unsettled: sent back, a change asked of a human who
    /// has not called it fine, or a change a human asked for.
    fn open(&self, answers: &BTreeMap<String, Answer>) -> bool {
        let answer = answers.get(&self.finding.id);
        let asked_change = self.asked
            && self.finding.lean == Lean::Change
            && !matches!(answer, Some(Answer::Fine));
        self.outcome.is_none()
            && (self.sent_back || asked_change || matches!(answer, Some(Answer::Change { .. })))
    }
}

/// What the next head's review says of the last review's open findings, by id: one it reports
/// `fine` again, by the same title, was settled as it says, or fixed if it does not say. One
/// reported again as a change, or not at all, stays open.
pub fn settled_by(
    last: &[Judged],
    answers: &BTreeMap<String, Answer>,
    now: &[Finding],
) -> Vec<(String, Outcome)> {
    last.iter()
        .filter(|j| j.open(answers))
        .filter_map(|j| {
            let again = now.iter().find(|f| f.same_as(&j.finding))?;
            (again.lean == Lean::Fine).then(|| {
                let settled = again.settled.unwrap_or(Settled::Fixed);
                (j.finding.id.clone(), settled.into())
            })
        })
        .collect()
}

/// The outcomes `after` gives that `before` had not, by title: what an edit of a review's
/// findings settled.
pub fn newly_settled(before: &[Judged], after: &[Judged]) -> Vec<(String, Outcome)> {
    after
        .iter()
        .zip(before)
        .filter(|(_, b)| b.outcome.is_none())
        .filter_map(|(a, _)| Some((a.finding.title.clone(), a.outcome?)))
        .collect()
}

/// An earlier review's findings, given what a later review of the PR settled, by title: an open
/// one raised again there was settled with it. Whether any changed.
pub fn carry_back(
    findings: &mut [Judged],
    answers: &BTreeMap<String, Answer>,
    settled: &[(String, Outcome)],
) -> bool {
    let mut changed = false;
    for j in findings.iter_mut().filter(|j| j.open(answers)) {
        if let Some((_, outcome)) = settled.iter().find(|(t, _)| j.finding.titled(t)) {
            j.settle(*outcome);
            changed = true;
        }
    }
    changed
}

/// What a human's answers settle: a change they called fine is dismissed.
pub fn settle_answers(findings: &mut [Judged], answers: &BTreeMap<String, Answer>) {
    for j in findings
        .iter_mut()
        .filter(|j| j.finding.lean == Lean::Change)
    {
        if let Some(Answer::Fine) = answers.get(&j.finding.id) {
            j.settle(Outcome::Dismissed);
        }
    }
}

/// The author resolved every thread with no push: what is still sent back was dismissed.
pub fn dismiss_sent_back(findings: &mut [Judged]) {
    for j in findings.iter_mut().filter(|j| j.sent_back) {
        j.settle(Outcome::Dismissed);
    }
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
                    outcome: None,
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
            settled: None,
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
        assert!(hole.wants_votes(Depth::Standard));
        assert_eq!(s.route(&hole), Route::Cleared);
        let panel = Panel::FINDING;
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
        assert!(!shown.wants_votes(Depth::Deep));
        assert_eq!(s.route(&shown), Route::Author);
        // At standard depth only a security change waits on votes: another kind needs evidence.
        let mut other = finding(Kind::Other, 100, Lean::Change);
        other.evidence = None;
        assert!(!other.wants_votes(Depth::Standard) && !other.wants_votes(Depth::Quick));
        assert_eq!(s.route(&other), Route::Cleared);
        // Nothing to vote on where the votes would not change the route.
        let mut call = hole.clone();
        call.needs_human = true;
        let mut fine = hole.clone();
        fine.lean = Lean::Fine;
        assert!(!call.wants_votes(Depth::Deep) && !fine.wants_votes(Depth::Deep));
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
            depth: None,
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

    fn judged(id: &str, title: &str, lean: Lean) -> Judged {
        let mut f = finding(Kind::Other, 90, lean);
        f.id = id.into();
        f.title = title.into();
        Judged {
            finding: f,
            asked: false,
            sent_back: false,
            withdrawn: false,
            threaded: false,
            bar: Bar::From(80),
            outcome: None,
        }
    }

    fn again(title: &str, lean: Lean, settled: Option<Settled>) -> Finding {
        let mut f = finding(Kind::Other, 90, lean);
        f.title = title.into();
        f.settled = settled;
        f
    }

    #[test]
    fn the_next_review_settles_what_the_last_held_the_pr_for() {
        let back = |id, title| Judged {
            sent_back: true,
            ..judged(id, title, Lean::Change)
        };
        let asked = |id, title| Judged {
            asked: true,
            ..judged(id, title, Lean::Change)
        };
        let last = vec![
            back("f1", "Retry never stops"),
            back("f2", "The lock races"),
            back("f3", "The cache leaks"),
            asked("f4", "The flag is public"),
            asked("f5", "The schema drops a column"),
            judged("f6", "Tests pass", Lean::Fine),
            Judged {
                outcome: Some(Outcome::Wrong),
                ..back("f7", "Withdrawn already")
            },
            back("f8", "Not raised again"),
        ];
        let answers = BTreeMap::from([
            (
                "f4".to_owned(),
                Answer::Change {
                    note: String::new(),
                },
            ),
            ("f5".to_owned(), Answer::Fine),
        ]);
        let now = vec![
            // The same title, however it is cased or spaced.
            again(" retry never stops", Lean::Fine, Some(Settled::Fixed)),
            again("The lock races", Lean::Fine, Some(Settled::Intended)),
            again("The cache leaks", Lean::Change, None),
            // Reported fine without saying why: the prompt asks a fixed one be reported so.
            again("The flag is public", Lean::Fine, None),
            again(
                "The schema drops a column",
                Lean::Fine,
                Some(Settled::Fixed),
            ),
            again("Tests pass", Lean::Fine, Some(Settled::Fixed)),
            again("Withdrawn already", Lean::Fine, Some(Settled::Fixed)),
        ];
        assert_eq!(
            settled_by(&last, &answers, &now),
            vec![
                ("f1".to_owned(), Outcome::Fixed),
                ("f2".to_owned(), Outcome::Intended),
                ("f4".to_owned(), Outcome::Fixed),
            ]
        );
    }

    #[test]
    fn a_later_reviews_outcome_carries_back_to_the_same_open_finding() {
        let back = |id, title| Judged {
            sent_back: true,
            ..judged(id, title, Lean::Change)
        };
        let before = vec![
            back("f1", "Retry never stops"),
            back("f2", "The lock races"),
        ];
        let mut after = before.clone();
        after[0].settle(Outcome::Wrong);
        let settled = newly_settled(&before, &after);
        assert_eq!(
            settled,
            vec![("Retry never stops".to_owned(), Outcome::Wrong)]
        );
        // Settled already: no news.
        assert!(newly_settled(&after, &after).is_empty());

        let mut earlier = vec![
            back("f3", " retry NEVER stops"),
            Judged {
                outcome: Some(Outcome::Fixed),
                ..back("f4", "Retry never stops")
            },
            back("f5", "The lock races"),
            // Not held for a change: nothing to settle.
            judged("f6", "Retry never stops", Lean::Change),
        ];
        assert!(carry_back(&mut earlier, &BTreeMap::new(), &settled));
        let outcomes: Vec<_> = earlier.iter().map(|j| j.outcome).collect();
        assert_eq!(
            outcomes,
            [Some(Outcome::Wrong), Some(Outcome::Fixed), None, None]
        );
        assert!(!carry_back(&mut earlier[2..], &BTreeMap::new(), &settled));
    }

    #[test]
    fn an_outcome_is_set_once() {
        let mut j = judged("f1", "t", Lean::Change);
        j.settle(Outcome::Wrong);
        j.settle(Outcome::Fixed);
        assert_eq!(j.outcome, Some(Outcome::Wrong));
    }

    #[test]
    fn a_human_who_calls_a_change_fine_dismisses_it() {
        let mut findings = vec![
            Judged {
                asked: true,
                ..judged("f1", "a", Lean::Change)
            },
            Judged {
                asked: true,
                ..judged("f2", "b", Lean::Change)
            },
            // A question about what the agent found fine is no change to dismiss.
            Judged {
                asked: true,
                ..judged("f3", "c", Lean::Fine)
            },
        ];
        let answers = BTreeMap::from([
            ("f1".to_owned(), Answer::Fine),
            (
                "f2".to_owned(),
                Answer::Change {
                    note: String::new(),
                },
            ),
            ("f3".to_owned(), Answer::Fine),
        ]);
        settle_answers(&mut findings, &answers);
        let outcomes: Vec<_> = findings.iter().map(|f| f.outcome).collect();
        assert_eq!(outcomes, vec![Some(Outcome::Dismissed), None, None]);
    }

    #[test]
    fn resolving_a_thread_with_no_push_dismisses_what_is_still_sent_back() {
        let mut findings = vec![
            Judged {
                sent_back: true,
                ..judged("f1", "a", Lean::Change)
            },
            Judged {
                withdrawn: true,
                outcome: Some(Outcome::Wrong),
                ..judged("f2", "b", Lean::Change)
            },
            judged("f3", "c", Lean::Change),
        ];
        dismiss_sent_back(&mut findings);
        let outcomes: Vec<_> = findings.iter().map(|f| f.outcome).collect();
        assert_eq!(
            outcomes,
            vec![Some(Outcome::Dismissed), Some(Outcome::Wrong), None]
        );
    }

    #[test]
    fn a_settled_finding_reads_as_the_agent_writes_it() {
        let read = |settled: serde_json::Value| -> Finding {
            serde_json::from_value(serde_json::json!({
                "kind": "other", "title": "t", "confidence": 90, "lean": "fine",
                "finding": "", "settled": settled
            }))
            .unwrap()
        };
        assert_eq!(read("wrong".into()).settled, Some(Settled::Wrong));
        // Anything else is no outcome, never a review that cannot be read: only a person
        // dismisses.
        for other in ["dismissed", "", "Fixed!"] {
            assert_eq!(read(other.into()).settled, None, "{other:?}");
        }
        assert_eq!(read(serde_json::Value::Null).settled, None);
        assert_eq!(read(3.into()).settled, None);
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

    #[test]
    fn a_deep_review_has_a_panel_vote_on_any_change_nothing_it_ran_shows() {
        use Verdict::*;
        let s = ReviewSettings {
            depth: Depth::Deep,
            ..ReviewSettings::default()
        };
        let mut other = finding(Kind::Other, 90, Lean::Change);
        other.evidence = None;
        assert!(other.wants_votes(Depth::Deep));
        assert_eq!(s.route(&other), Route::Cleared);
        other.verified = Some(Panel::FINDING.verdict(votes(&[Holds, Holds, DoesNotHold])));
        assert_eq!(s.route(&other), Route::Author);
        other.verified = Some(Panel::FINDING.verdict(votes(&[Holds, DoesNotHold, Abstained])));
        assert_eq!(s.route(&other), Route::Cleared);
        // Its bar still holds: upheld, but under it, is a note.
        other.confidence = 70;
        other.verified = Some(Panel::FINDING.verdict(votes(&[Holds, Holds, Holds])));
        assert_eq!(s.route(&other), Route::Cleared);
    }

    #[test]
    fn depth_is_standard_unless_the_workspace_or_the_repo_sets_it() {
        assert_eq!(ReviewSettings::default().depth, Depth::Standard);
        // Settings stored before depth existed read as standard.
        let stored = serde_json::json!({ "enabled": true, "bars": {} });
        let old: ReviewSettings = serde_json::from_value(stored).unwrap();
        assert_eq!(old.depth, Depth::Standard);
        let ws = ReviewSettings {
            depth: Depth::Quick,
            ..ReviewSettings::default()
        };
        assert_eq!(ws.with(&ReviewOverrides::default()).depth, Depth::Quick);
        let o: ReviewOverrides = serde_yaml::from_str("depth: deep").unwrap();
        assert!(!o.is_empty());
        assert_eq!(ws.with(&o).depth, Depth::Deep);
        assert_eq!(serde_yaml::to_string(&o).unwrap(), "depth: deep\n");
        assert!(serde_yaml::from_str::<ReviewOverrides>("depth: thorough").is_err());
        assert_eq!(
            Depth::ALL.map(|d| (d.as_str(), d.reviewers())),
            [("quick", 1), ("standard", 1), ("deep", 3)]
        );
    }

    fn at(title: &str, kind: Kind, file: &str, line: u32, lean: Lean, confidence: u8) -> Finding {
        Finding {
            title: title.into(),
            file: Some(file.into()),
            line: Some(line),
            evidence: None,
            ..finding(kind, confidence, lean)
        }
    }

    #[test]
    fn reviewers_findings_merge_one_per_issue_in_the_order_first_found() {
        let a = vec![
            at(
                "Retry never stops",
                Kind::Other,
                "src/retry.rs",
                40,
                Lean::Change,
                70,
            ),
            at(
                "Token compared with ==",
                Kind::Security,
                "src/auth.rs",
                12,
                Lean::Change,
                80,
            ),
        ];
        let mut shown = at(
            "The retry loop has no cap",
            Kind::Other,
            "src/retry.rs",
            42,
            Lean::Change,
            60,
        );
        shown.evidence = Some(Evidence {
            command: "cargo test retry".into(),
            ..Evidence::default()
        });
        let b = vec![
            // The same bug, two lines on, with evidence: that one speaks for it.
            shown.clone(),
            // The same line, another kind: another issue.
            at(
                "Retry logs the token",
                Kind::Security,
                "src/retry.rs",
                40,
                Lean::Change,
                75,
            ),
        ];
        let c = vec![
            // A sound note in the same place as a bug: the bug is what matters there.
            at(
                "Retry is sound",
                Kind::Other,
                "src/retry.rs",
                41,
                Lean::Fine,
                95,
            ),
            // Far enough away to be another issue.
            at(
                "Backoff overflows",
                Kind::Other,
                "src/retry.rs",
                90,
                Lean::Change,
                85,
            ),
            at(
                "Token compared with ==",
                Kind::Security,
                "src/auth.rs",
                12,
                Lean::Change,
                90,
            ),
        ];
        let merged = merge_reviews(vec![a, b, c]);
        let titles: Vec<&str> = merged.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "The retry loop has no cap",
                "Token compared with ==",
                "Retry logs the token",
                "Backoff overflows",
            ]
        );
        assert_eq!(merged[0], shown);
        // Of two alike, the surer one.
        assert_eq!(merged[1].confidence, 90);
    }

    #[test]
    fn a_bug_found_at_several_places_merges_with_one_found_at_any_of_them() {
        let mut class = at(
            "Unchecked unwrap",
            Kind::Other,
            "a.rs",
            10,
            Lean::Change,
            80,
        );
        class.also_at = vec![Place {
            file: "b.rs".into(),
            line: Some(50),
            code: Vec::new(),
        }];
        let one = at(
            "Panics on empty input",
            Kind::Other,
            "b.rs",
            51,
            Lean::Change,
            90,
        );
        let merged = merge_reviews(vec![vec![class], vec![one]]);
        assert_eq!(merged.len(), 1);
        // The surer one stands for it, and keeps every place, so one fix settles them all.
        assert_eq!(merged[0].title, "Panics on empty input");
        let also: Vec<String> = merged[0].also_at.iter().map(|p| p.to_string()).collect();
        assert_eq!(also, ["a.rs:10"]);
    }

    #[test]
    fn a_cap_on_findings_drops_notes_before_any_change() {
        let note = |i: u32| {
            at(
                &format!("sound {i}"),
                Kind::Other,
                "a.rs",
                i * 10,
                Lean::Fine,
                90,
            )
        };
        let mut findings: Vec<Finding> = (1..=25).map(note).collect();
        findings.push(at(
            "Token leaks",
            Kind::Security,
            "b.rs",
            1,
            Lean::Change,
            80,
        ));
        findings.push(at("Old bug", Kind::Other, "c.rs", 1, Lean::PreExisting, 80));
        let kept = most_first(findings, 25);
        assert_eq!(kept.len(), 25);
        // In the order found, without the last notes.
        assert_eq!(kept[0].title, "sound 1");
        assert_eq!(kept[22].title, "sound 23");
        assert_eq!(kept[23].title, "Token leaks");
        assert_eq!(kept[24].title, "Old bug");
        let few = vec![note(1), note(2)];
        assert_eq!(most_first(few.clone(), 25), few);
    }

    #[test]
    fn findings_about_a_whole_file_or_pr_merge_only_with_the_same_title() {
        let whole = |title: &str| Finding {
            title: title.into(),
            ..finding(Kind::Other, 80, Lean::Change)
        };
        let file = |title: &str| Finding {
            file: Some("Cargo.toml".into()),
            ..whole(title)
        };
        let merged = merge_reviews(vec![
            vec![
                whole("No test covers the retry"),
                file("A crate is pinned to git"),
            ],
            vec![
                whole("No test covers the  retry."),
                whole("The PR does more than it says"),
                file("A crate is pinned to git"),
            ],
        ]);
        let titles: Vec<&str> = merged.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "No test covers the retry",
                "A crate is pinned to git",
                "The PR does more than it says"
            ]
        );
        // A reviewer's own findings are as many issues as it says, however close.
        let one = vec![whole("Twice"), whole("Twice")];
        assert_eq!(merge_reviews(vec![one.clone()]), one);
        let near = |line| at("x", Kind::Other, "a.rs", line, Lean::Change, 80);
        let merged = merge_reviews(vec![vec![near(1), near(2)], vec![near(3)]]);
        assert_eq!(merged.len(), 2);
    }
}
