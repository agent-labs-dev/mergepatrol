//! The issues a PR closes. GitHub closes an issue named after a closing keyword in the PR's
//! body, or in a commit message that lands on the default branch. A squash whose message drops
//! the commit bodies loses the second, so the queue copies those into the body before it merges.

use std::collections::BTreeSet;

const KEYWORDS: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// The issues `text` closes, each as `#N` for one in `repo` or `owner/name#N` for another.
/// GitHub reads no keyword inside code, fenced or inline.
fn closed_in(text: &str, repo: &str) -> BTreeSet<String> {
    let prose = without_code(text);
    let words: Vec<&str> = prose.split_whitespace().collect();
    words
        .windows(2)
        .filter(|w| {
            let word = w[0].trim_end_matches(':').to_ascii_lowercase();
            KEYWORDS.contains(&word.as_str())
        })
        .filter_map(|w| issue_ref(w[1], repo))
        .collect()
}

/// `text` with its fenced blocks and inline code spans blanked out.
fn without_code(text: &str) -> String {
    let mut prose = String::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            // Odd-numbered pieces between backticks are code.
            prose.extend(line.split('`').step_by(2).flat_map(|p| [p, " "]));
        }
        prose.push('\n');
    }
    prose
}

fn issue_ref(word: &str, repo: &str) -> Option<String> {
    let word = word.trim_end_matches([',', '.', ';', ')', ']']);
    let (owner_name, number) = word.split_once('#')?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if owner_name.is_empty() || owner_name.eq_ignore_ascii_case(repo) {
        return Some(format!("#{number}"));
    }
    let (owner, name) = owner_name.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some(format!("{}#{number}", owner_name.to_ascii_lowercase()))
}

/// `body` with a `Closes` line for each issue a commit in `messages` closes and `body` does not,
/// or `None` when it names them all. `repo` is `owner/name`.
pub fn body_closing_commit_issues<'a>(
    body: &str,
    messages: impl IntoIterator<Item = &'a str>,
    repo: &str,
) -> Option<String> {
    let named = closed_in(body, repo);
    let missing: BTreeSet<String> = messages
        .into_iter()
        .flat_map(|m| closed_in(m, repo))
        .filter(|r| !named.contains(r))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let lines: Vec<String> = missing.iter().map(|r| format!("Closes {r}")).collect();
    let body = body.trim_end();
    let sep = if body.is_empty() { "" } else { "\n\n" };
    Some(format!("{body}{sep}{}", lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_what_only_a_commit_closes() {
        let cases: [(&str, &[&str], Option<&str>); 12] = [
            (
                "What changed.",
                &["feat: x\n\nFixes #4371"],
                Some("What changed.\n\nCloses #4371"),
            ),
            (
                "",
                &["fix: y\n\ncloses: #2, resolves o/other#3."],
                Some("Closes #2\nCloses o/other#3"),
            ),
            ("Closes #5", &["fix: z\n\nFixes #5"], None),
            ("Fixes O/R#5", &["fix: z\n\nFixes #5"], None),
            ("Body", &["fix: z\n\nRefs #5, see #6"], None),
            ("Body", &["Fixes the #5 bug"], None),
            ("Body", &["Fixes #abc", "Fixes #"], None),
            (
                "Body",
                &["Fixes #7", "Fixed o/r#7"],
                Some("Body\n\nCloses #7"),
            ),
            ("Body\n\n", &["prefix fixes #9"], Some("Body\n\nCloses #9")),
            (
                "Say `Fixes #4`",
                &["Fixes #4"],
                Some("Say `Fixes #4`\n\nCloses #4"),
            ),
            (
                "```\nFixes #4\n```",
                &["Fixes #4"],
                Some("```\nFixes #4\n```\n\nCloses #4"),
            ),
            ("Body", &["Use `Fixes #4` here"], None),
        ];
        for (body, messages, want) in cases {
            let got = body_closing_commit_issues(body, messages.iter().copied(), "o/r");
            assert_eq!(got.as_deref(), want, "body {body:?}, messages {messages:?}");
        }
    }
}
