//! The trunk's code owners, for the review GitHub requires of them. When the trunk's rules require
//! a code owner's review but no approval count besides, GitHub leaves `reviewDecision` empty, though
//! it will not merge a PR that changes an owned file without its owner's approval.

use serde_json::Value;

/// What the repository query reads of the trunk, for [`CodeOwners::from_repository`]: its name, its
/// rules, and CODEOWNERS where GitHub looks for it, in its order.
pub(crate) const FIELDS: &str = "defaultBranchRef { name rules(first: 50) { nodes { parameters { ... on PullRequestParameters { requireCodeOwnerReview } } } } }
  codeowners0: object(expression: \"HEAD:.github/CODEOWNERS\") { ... on Blob { text } }
  codeowners1: object(expression: \"HEAD:CODEOWNERS\") { ... on Blob { text } }
  codeowners2: object(expression: \"HEAD:docs/CODEOWNERS\") { ... on Blob { text } }";

/// The trunk's CODEOWNERS, as GitHub reads it: the last line that matches a path names its owners.
#[derive(Debug, Default)]
pub struct CodeOwners {
    /// The branch whose rules require the review: the default branch.
    pub branch: String,
    lines: Vec<(String, Vec<String>)>,
}

impl CodeOwners {
    /// The code owners whose review the trunk's rules require, if they require one.
    pub(crate) fn from_repository(repo: &Value) -> Option<Self> {
        let required = repo["defaultBranchRef"]["rules"]["nodes"]
            .as_array()?
            .iter()
            .any(|r| r["parameters"]["requireCodeOwnerReview"] == true);
        let text = (0..3).find_map(|i| repo[format!("codeowners{i}")]["text"].as_str())?;
        let branch = repo["defaultBranchRef"]["name"].as_str()?;
        required.then(|| Self::parse(branch, text))
    }

    pub fn parse(branch: &str, text: &str) -> Self {
        let lines = text
            .lines()
            .filter_map(|line| {
                let mut words = line.split_whitespace();
                let pattern = words.next().filter(|w| !w.starts_with('#'))?;
                let owners = words.take_while(|w| !w.starts_with('#'));
                Some((pattern.to_owned(), owners.map(str::to_owned).collect()))
            })
            .collect();
        CodeOwners {
            branch: branch.to_owned(),
            lines,
        }
    }

    /// The owners of a path: empty when it has none.
    fn owners(&self, path: &str) -> &[String] {
        self.lines
            .iter()
            .rev()
            .find(|(pattern, _)| matches(pattern, path))
            .map_or(&[], |(_, owners)| owners)
    }

    /// Whether a changed file still needs its owner's approval. An approval counts for a file
    /// owned by a team or an email address: the App cannot read who is in them.
    pub fn awaits_review(&self, files: &[String], approvers: &[&str]) -> bool {
        files.iter().any(|path| {
            let owners = self.owners(path);
            !owners.is_empty()
                && !owners.iter().any(|owner| match owner.strip_prefix('@') {
                    Some(login) if !login.contains('/') => {
                        approvers.iter().any(|a| a.eq_ignore_ascii_case(login))
                    }
                    _ => !approvers.is_empty(),
                })
        })
    }
}

/// Whether a CODEOWNERS pattern matches a file. As in `.gitignore`, a pattern with a `/` before
/// its end is anchored at the root, and one that matches a directory matches all under it. Unlike
/// it, a pattern that ends in `/*` matches only the directory's own files.
fn matches(pattern: &str, path: &str) -> bool {
    let (pattern, dir_only) = match pattern.strip_suffix('/') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let anchored = pattern.contains('/');
    let pattern: Vec<&str> = pattern.trim_start_matches('/').split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    let below = pattern.last() != Some(&"*");
    if anchored {
        segments(&pattern, &path, dir_only, below)
    } else {
        (0..path.len()).any(|start| segments(&pattern, &path[start..], dir_only, below))
    }
}

/// Whether `pattern` matches the start of `path`: all of it, a file, or a directory above the rest.
fn segments(pattern: &[&str], path: &[&str], dir_only: bool, below: bool) -> bool {
    match pattern.split_first() {
        None => {
            if path.is_empty() {
                !dir_only
            } else {
                below
            }
        }
        Some((&"**", rest)) => {
            (0..=path.len()).any(|skip| segments(rest, &path[skip..], dir_only, below))
        }
        Some((first, rest)) => path.split_first().is_some_and(|(name, path)| {
            glob(first.as_bytes(), name.as_bytes()) && segments(rest, path, dir_only, below)
        }),
    }
}

/// `*` and `?` within one path segment.
fn glob(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|skip| glob(rest, &name[skip..])),
        Some((b'?', rest)) => !name.is_empty() && glob(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && glob(rest, &name[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The examples of GitHub's CODEOWNERS docs.
    #[test]
    fn patterns_match_as_github_documents() {
        let cases = [
            ("*", "a/b.txt", true),
            ("*.js", "src/deep/app.js", true),
            ("*.js", "src/app.jsx", false),
            ("*.go", "main.go", true),
            ("/build/logs/", "build/logs/x/y.log", true),
            ("/build/logs/", "src/build/logs/y.log", false),
            ("docs/*", "docs/getting-started.md", true),
            ("docs/*", "docs/build-app/troubleshooting.md", false),
            ("apps/", "web/apps/x.ts", true),
            ("apps/", "apps", false),
            ("/docs/", "docs/a/b.md", true),
            ("/scripts/", "scripts/a.sh", true),
            ("/scripts/", "src/scripts/a.sh", false),
            ("**/logs", "build/logs/a.log", true),
            ("**/logs", "deeply/nested/logs/a.log", true),
            ("**/logs", "logs/a.log", true),
            ("/apps/github", "apps/github/a.rb", true),
            ("/apps/github", "apps/githubx/a.rb", false),
            ("*cloudbuild*.yaml", "deploy/x/noop.cloudbuild.yaml", true),
            ("/skaffold.yaml", "skaffold.yaml", true),
            ("/skaffold.yaml", "sub/skaffold.yaml", false),
        ];
        for (pattern, path, want) in cases {
            assert_eq!(matches(pattern, path), want, "{pattern} on {path}");
        }
    }

    const NEBULA: &str = "# Release-control files.\n\n\
        /.github/                   @joaquim-verges @jnsdls @furqanrydhan @cjber\n\
        *cloudbuild*.yaml           @joaquim-verges @jnsdls\n\
        /apps/                      @org/team\n\
        /apps/public/               # no owner\n";

    fn files(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn an_owned_file_awaits_one_of_its_owners() {
        let owners = CodeOwners::parse("main", NEBULA);
        let workflow = files(&[".github/workflows/maintainability.yml"]);
        assert!(owners.awaits_review(&workflow, &[]));
        assert!(owners.awaits_review(&workflow, &["someone-else"]));
        assert!(!owners.awaits_review(&workflow, &["JNSDLS"]));
        // Every owned file needs one of its own owners.
        let both = files(&[".github/x.yml", "cloudbuild.yaml"]);
        assert!(owners.awaits_review(&both, &["cjber"]));
        assert!(!owners.awaits_review(&both, &["jnsdls"]));
    }

    #[test]
    fn files_without_an_owner_await_no_one() {
        let owners = CodeOwners::parse("main", NEBULA);
        assert!(!owners.awaits_review(&files(&["src/models/postgres/user.py"]), &[]));
        // The last match wins, and it can name no owner.
        assert!(!owners.awaits_review(&files(&["apps/public/a.ts"]), &[]));
    }

    #[test]
    fn any_approval_counts_for_a_team() {
        let owners = CodeOwners::parse("main", NEBULA);
        let app = files(&["apps/a.ts"]);
        assert!(owners.awaits_review(&app, &[]));
        assert!(!owners.awaits_review(&app, &["ann"]));
    }

    #[test]
    fn only_a_rule_that_requires_code_owners_reads_them() {
        let repo = |required: bool| {
            json!({
                "defaultBranchRef": { "name": "main", "rules": { "nodes": [
                    { "parameters": null },
                    { "parameters": { "requireCodeOwnerReview": required } },
                ] } },
                "codeowners0": null,
                "codeowners1": { "text": "* @ann" },
                "codeowners2": null,
            })
        };
        assert!(CodeOwners::from_repository(&repo(true)).is_some());
        assert!(CodeOwners::from_repository(&repo(false)).is_none());
    }
}
