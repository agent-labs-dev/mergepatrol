//! An eval case: a PR as it was when it merged, and the bugs it shipped, or none. One file in
//! `evals/review/cases/`, named for its id. A case is checked when it is read, so the rest of
//! the eval trusts it.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    /// The file's name, without `.yml`.
    #[serde(skip)]
    pub id: String,
    /// `owner/name`, a public repo on GitHub.
    pub repo: String,
    pub pr: u64,
    /// The branch it merged into.
    #[serde(default = "main")]
    pub trunk: String,
    /// Where the PR left the trunk: its changes are `base..head`.
    pub base: Sha,
    /// The PR's head as it merged.
    pub head: Sha,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// The repo's verify commands when it merged, from its config file. None has the agent run
    /// what the repo's CI runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verify: Vec<String>,
    pub expected: Expected,
}

fn main() -> String {
    "main".into()
}

/// What a careful reviewer should have sent back on the PR. Written `clean`, or `bugs:` and a
/// list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "ExpectedText", into = "ExpectedText")]
pub enum Expected {
    /// Nothing: any change sent back is a false positive.
    Clean,
    Bugs(Bugs),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ExpectedText {
    Clean(CleanWord),
    Bugs { bugs: Bugs },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CleanWord {
    Clean,
}

impl From<ExpectedText> for Expected {
    fn from(text: ExpectedText) -> Self {
        match text {
            ExpectedText::Clean(CleanWord::Clean) => Expected::Clean,
            ExpectedText::Bugs { bugs } => Expected::Bugs(bugs),
        }
    }
}

impl From<Expected> for ExpectedText {
    fn from(e: Expected) -> Self {
        match e {
            Expected::Clean => ExpectedText::Clean(CleanWord::Clean),
            Expected::Bugs(bugs) => ExpectedText::Bugs { bugs },
        }
    }
}

/// At least one: of a case's bugs, or of the places a bug lives. A case with no bugs is
/// `clean`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<T>", into = "Vec<T>")]
#[serde(bound(
    serialize = "T: Clone + Serialize",
    deserialize = "T: Deserialize<'de>"
))]
pub struct NonEmpty<T>(Vec<T>);

impl<T> NonEmpty<T> {
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.0.iter()
    }
}

impl<T> TryFrom<Vec<T>> for NonEmpty<T> {
    type Error = &'static str;

    fn try_from(items: Vec<T>) -> Result<Self, Self::Error> {
        if items.is_empty() {
            return Err("the list is empty");
        }
        Ok(NonEmpty(items))
    }
}

impl<T> From<NonEmpty<T>> for Vec<T> {
    fn from(items: NonEmpty<T>) -> Self {
        items.0
    }
}

pub type Bugs = NonEmpty<Bug>;

/// A bug the PR shipped, where it lives on the PR's head, and what fixed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bug {
    /// Each place the fix had to change: one bug may span a few.
    pub at: NonEmpty<Place>,
    pub what: String,
    /// The PR or commit that fixed it, e.g. `#193`.
    pub fixed_by: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Place {
    pub file: String,
    pub lines: Lines,
}

/// Lines `start` to `end` of a file, both included, counted from 1. Written `40-52`, or `40`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "LinesText", into = "String")]
pub struct Lines {
    start: u32,
    end: u32,
}

impl Lines {
    pub fn new(start: u32, end: u32) -> Result<Self, String> {
        if start == 0 || end < start {
            return Err(format!("{start}-{end} is not a range of lines"));
        }
        Ok(Lines { start, end })
    }

    pub fn start(self) -> u32 {
        self.start
    }

    pub fn end(self) -> u32 {
        self.end
    }
}

/// YAML reads `40` as a number.
#[derive(Deserialize)]
#[serde(untagged)]
enum LinesText {
    One(u32),
    Range(String),
}

impl TryFrom<LinesText> for Lines {
    type Error = String;

    fn try_from(text: LinesText) -> Result<Self, String> {
        let text = match text {
            LinesText::One(n) => return Lines::new(n, n),
            LinesText::Range(text) => text,
        };
        let (start, end) = text.split_once('-').unwrap_or((&text, &text));
        let n = |s: &str| {
            s.trim()
                .parse::<u32>()
                .map_err(|_| format!("{text:?} is not `40` or `40-52`"))
        };
        Lines::new(n(start)?, n(end)?)
    }
}

impl From<Lines> for String {
    fn from(l: Lines) -> Self {
        if l.start == l.end {
            l.start.to_string()
        } else {
            format!("{}-{}", l.start, l.end)
        }
    }
}

/// A full commit SHA.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha(String);

impl Sha {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Sha {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        if s.len() != 40 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{s:?} is not a full commit SHA"));
        }
        Ok(Sha(s.to_ascii_lowercase()))
    }
}

impl From<Sha> for String {
    fn from(s: Sha) -> Self {
        s.0
    }
}

impl Case {
    pub fn parse(id: &str, yaml: &str) -> Result<Case> {
        let mut case: Case = serde_yaml::from_str(yaml).with_context(|| format!("case {id}"))?;
        if case.repo.split('/').filter(|p| !p.is_empty()).count() != 2 {
            bail!("case {id}: {:?} is not `owner/name`", case.repo);
        }
        case.id = id.to_owned();
        Ok(case)
    }
}

/// Every case in `dir`, by id.
pub fn load(dir: &Path) -> Result<Vec<Case>> {
    let mut cases = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "yml") {
            continue;
        }
        let id = path.file_stem().unwrap_or_default().to_string_lossy();
        cases.push(Case::parse(&id, &std::fs::read_to_string(&path)?)?);
    }
    cases.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(cases)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn yaml(expected: &str) -> String {
        format!(
            "repo: o/r\npr: 7\nbase: {SHA_A}\nhead: {SHA_B}\ntitle: t\nbody: b\nexpected:\n{expected}"
        )
    }

    #[test]
    fn a_case_reads_its_bugs_and_where_they_live() {
        let case = Case::parse(
            "pr-7",
            &yaml("  bugs:\n    - at:\n        - { file: src/a.rs, lines: 40-52 }\n        - { file: b.rs, lines: 3 }\n      what: w\n      fixed_by: '#9'\n    - at: [{ file: c.rs, lines: 7 }]\n      what: w\n      fixed_by: '#9'\n"),
        )
        .unwrap();
        assert_eq!(case.id, "pr-7");
        let Expected::Bugs(bugs) = &case.expected else {
            panic!("not bugs");
        };
        let places: Vec<Vec<_>> = bugs
            .iter()
            .map(|b| {
                b.at.iter()
                    .map(|p| (p.file.as_str(), p.lines.start(), p.lines.end()))
                    .collect()
            })
            .collect();
        assert_eq!(
            places,
            [
                vec![("src/a.rs", 40, 52), ("b.rs", 3, 3)],
                vec![("c.rs", 7, 7)]
            ]
        );
    }

    #[test]
    fn a_clean_case_has_no_bugs() {
        let case = Case::parse("c", &yaml("  clean\n")).unwrap();
        assert_eq!(case.expected, Expected::Clean);
        let case = Case::parse("c", &yaml("  clean")).unwrap();
        assert_eq!(case.expected, Expected::Clean);
    }

    #[test]
    fn a_case_that_could_not_be_scored_does_not_load() {
        let bug = |lines: &str| {
            yaml(&format!(
                "  bugs:\n    - at: [{{ file: a, lines: {lines} }}]\n      what: w\n      fixed_by: x\n"
            ))
        };
        for bad in [
            yaml("  bugs: []\n"),
            yaml("  bugs:\n    - at: []\n      what: w\n      fixed_by: x\n"),
            bug("52-40"),
            bug("0"),
            bug("x"),
            yaml("  clean\n").replace(SHA_A, "abc123"),
            yaml("  clean\n").replace("o/r", "r"),
            yaml("  clean\n") + "extra: 1\n",
        ] {
            assert!(Case::parse("c", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_dir_loads_its_cases_by_id_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.yml"), yaml("  clean\n")).unwrap();
        std::fs::write(dir.path().join("a.yml"), yaml("  clean\n")).unwrap();
        std::fs::write(dir.path().join("README.md"), "not a case").unwrap();
        let ids: Vec<_> = load(dir.path())
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, ["a", "b"]);

        std::fs::write(dir.path().join("c.yml"), "repo: x").unwrap();
        assert!(load(dir.path()).is_err());
        assert!(load(&dir.path().join("gone")).is_err());
    }

    #[test]
    fn every_case_in_the_repo_loads() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/review/cases");
        let cases = load(&dir).unwrap();
        assert!(cases.len() >= 20, "{} cases", cases.len());
        assert!(cases.iter().any(|c| c.expected == Expected::Clean));
    }

    #[test]
    fn a_case_writes_back_as_it_reads() {
        let case = Case::parse(
            "c",
            &yaml(
                "  bugs:\n    - at: [{ file: a, lines: 4-9 }]\n      what: w\n      fixed_by: x\n",
            ),
        )
        .unwrap();
        let again = Case::parse("c", &serde_yaml::to_string(&case).unwrap()).unwrap();
        assert_eq!(again, case);
    }
}
