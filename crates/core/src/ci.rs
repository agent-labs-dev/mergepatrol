//! The checks a repo runs, read from its own CI.
//!
//! A repo that sets no `verify` commands still has checks: the ones its workflows run for a pull
//! request. MergeLoop reads them from the checkout a job already has, so a review has a command
//! it can make fail to show a change is needed, and the fix agent knows what to run. MergeLoop
//! never runs these to gate a merge, so a command that cannot run outside CI costs nothing: it is
//! left out here.

/// The most commands kept: enough for a repo's checks, few enough to read in a prompt.
pub const MAX: usize = 5;

/// What a workflow's command needs, and a job's checkout cannot give it: the workflow's context
/// or a secret, or a deployment of its own.
const NEEDS_CI: [&str; 8] = [
    "${{",
    "secrets.",
    "deploy",
    "publish",
    "release",
    "container",
    "$(",
    "docker ",
];

/// A tool that only CI has, or that reaches outside the checkout.
const NEEDS_A_HOST: [&str; 6] = ["gh ", "ssh ", "kubectl", "aws ", "gcloud", "terraform"];

/// A shell line that only joins with others inside `if`, `for` or `while`.
const CONTROL_FLOW: [&str; 7] = ["if ", "for ", "while ", "then", "else", "do", "done"];

/// The checks `files` (a repo's workflows, by path) run for a pull request, in the order they
/// appear. Empty when none of them can run outside CI.
pub fn verify_from_ci(files: &[(String, String)]) -> Vec<String> {
    let mut commands: Vec<String> = Vec::new();
    for (path, yaml) in files {
        if !is_workflow(path) || !runs_for_a_pull_request(yaml) {
            continue;
        }
        for command in run_commands(yaml) {
            if !runs_in_a_checkout(&command) || commands.contains(&command) {
                continue;
            }
            commands.push(command);
            if commands.len() == MAX {
                return commands;
            }
        }
    }
    commands
}

fn is_workflow(path: &str) -> bool {
    path.ends_with(".yml") || path.ends_with(".yaml")
}

/// Whether a workflow runs on a pull request, whose head is what a review can check.
fn runs_for_a_pull_request(yaml: &str) -> bool {
    yaml.contains("pull_request")
}

/// The commands a workflow's steps run: a one-line `run:`, or a `run: |` block joined with
/// `&&`, run from the step's `working-directory` or the inherited `defaults.run` one. A block
/// that holds control flow is left out: its lines do not join that way.
fn run_commands(yaml: &str) -> Vec<String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut commands = Vec::new();
    // The `defaults.run.working-directory` in effect, by the indent of its `defaults:` key.
    let mut defaults: Vec<(usize, String)> = Vec::new();
    let mut defaults_indent = 0;
    // The step being read: the indent of its keys, its command and its working directory.
    let mut step: Option<(usize, Option<String>, Option<String>)> = None;
    let finish = |step: Option<(usize, Option<String>, Option<String>)>,
                  defaults: &[(usize, String)],
                  commands: &mut Vec<String>| {
        if let Some((_, Some(run), dir)) = step {
            match dir.or_else(|| defaults.last().map(|(_, d)| d.clone())) {
                Some(dir) => commands.push(format!("cd {dir} && {run}")),
                None => commands.push(run),
            }
        }
    };
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - trimmed.len();
        if step.as_ref().is_some_and(|(k, _, _)| indent < *k) {
            finish(step.take(), &defaults, &mut commands);
        }
        defaults.retain(|(d, _)| indent >= *d);
        let item = trimmed.strip_prefix("- ");
        let key = item.unwrap_or(trimmed).trim_start();
        // The indent of this line's key, past any sequence marker.
        let key_indent = line.len() - key.len();
        // A sequence item starts a step, unless it is a list nested inside the current one.
        if item.is_some() && step.as_ref().is_none_or(|(k, _, _)| key_indent <= *k) {
            finish(step.take(), &defaults, &mut commands);
            step = Some((key_indent, None, None));
        }
        if key.starts_with("defaults:") {
            defaults_indent = key_indent;
        }
        if let Some(dir) = key.strip_prefix("working-directory:") {
            let dir = scalar(dir.trim());
            match step.as_mut() {
                Some((k, _, d)) if *k == key_indent => *d = Some(dir),
                Some(_) => {}
                None => defaults.push((defaults_indent, dir)),
            }
            continue;
        }
        let Some(value) = key.strip_prefix("run:") else {
            continue;
        };
        let Some((k, run, _)) = step.as_mut() else {
            continue;
        };
        if *k != key_indent {
            continue;
        }
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if !value.starts_with(['|', '>']) {
            *run = Some(scalar(value));
            continue;
        }
        // A block: the lines under it, indented past the `run:` key.
        let mut block: Vec<String> = Vec::new();
        while i < lines.len() {
            let next = lines[i];
            let text = next.trim();
            if text.is_empty() {
                i += 1;
                continue;
            }
            if next.len() - next.trim_start().len() <= key_indent {
                break;
            }
            let line_text = strip_inline_comment(text);
            if line_text.is_empty() {
                i += 1;
                continue;
            }
            block.push(line_text);
            i += 1;
        }
        // A folded block (`>`) is one command YAML joins with spaces, not lines to chain; skip it.
        if value.starts_with('>')
            || block.iter().any(|l| l.starts_with('#'))
            || holds_control_flow(&block)
        {
            continue;
        }
        *run = Some(block.join(" && "));
    }
    finish(step, &defaults, &mut commands);
    commands
}

/// A one-line YAML scalar's value: a quoted scalar unquoted, a plain one as it is.
fn scalar(value: &str) -> String {
    let quoted = |q: char| value.len() >= 2 && value.starts_with(q) && value.ends_with(q);
    if quoted('\'') {
        value[1..value.len() - 1].replace("''", "'")
    } else if quoted('"') {
        value[1..value.len() - 1]
            .replace("\\\\", "\0")
            .replace("\\\"", "\"")
            .replace('\0', "\\")
    } else {
        value.to_owned()
    }
}

/// A shell line with its trailing comment removed. Joining lines with `&&` would otherwise let an
/// inline `#` comment swallow every command after it. A `#` inside a quote is kept.
fn strip_inline_comment(line: &str) -> String {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        // A backslash escapes the next character, except inside single quotes.
        if escaped {
            escaped = false;
            continue;
        }
        match quote {
            Some(q) => {
                if c == '\\' && q == '"' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\\' => escaped = true,
                '\'' | '"' => quote = Some(c),
                '#' if i == 0 || line[..i].ends_with(char::is_whitespace) => {
                    return line[..i].trim_end().to_owned();
                }
                _ => {}
            },
        }
    }
    line.to_owned()
}

fn holds_control_flow(lines: &[String]) -> bool {
    lines.iter().any(|l| {
        let l = l.trim_start();
        CONTROL_FLOW.iter().any(|c| l.starts_with(c)) || l.ends_with('\\')
    })
}

/// Whether a command can run in a job's checkout, which has the toolchains but no CI context,
/// no secret and no host of its own.
fn runs_in_a_checkout(command: &str) -> bool {
    let lower = command.to_lowercase();
    let needed = NEEDS_CI.iter().chain(NEEDS_A_HOST.iter());
    if needed.into_iter().any(|n| lower.contains(n)) {
        return false;
    }
    // A step that only prints is not a check: keep it only when one of its commands is a check,
    // so a status message ahead of the tests does not discard them.
    command
        .split("&&")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .any(|c| {
            let first = c.split_whitespace().next().unwrap_or_default();
            // Only a lone `echo` or `printf` is known to just print: one that a `;`, `|` or `&`
            // joins to another command may run a check after it.
            (first != "echo" && first != "printf") || c.contains([';', '|', '&', '\n', '`'])
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This repo's own CI: a `run: |` block, then one-line steps.
    const CI: &str = "\
name: ci

on:
  pull_request:
  push:
    branches: [main]

jobs:
  check:
    runs-on: blacksmith-4vcpu-ubuntu-2404
    services:
      postgres:
        image: postgres:17-alpine
    steps:
      - uses: actions/checkout@v7
      - name: ui
        working-directory: ui
        run: |
          corepack enable
          pnpm install --frozen-lockfile
          pnpm check
      - run: cargo fmt --all --check
      - run: cargo test --workspace --locked
";

    fn files(yaml: &str) -> Vec<(String, String)> {
        vec![("ci.yml".to_owned(), yaml.to_owned())]
    }

    #[test]
    fn a_block_joins_its_lines_and_a_one_line_step_keeps_its_own() {
        assert_eq!(
            verify_from_ci(&files(CI)),
            [
                "cd ui && corepack enable && pnpm install --frozen-lockfile && pnpm check",
                "cargo fmt --all --check",
                "cargo test --workspace --locked",
            ]
        );
    }

    #[test]
    fn a_step_runs_from_the_working_directory_its_job_defaults_to() {
        let yaml = "\
on: pull_request
jobs:
  ui:
    defaults:
      run:
        working-directory: ui
    steps:
      - run: pnpm check
  core:
    steps:
      - run: cargo test
";
        assert_eq!(
            verify_from_ci(&files(yaml)),
            ["cd ui && pnpm check", "cargo test"]
        );
    }

    #[test]
    fn a_workflow_that_never_runs_on_a_pull_request_is_no_check_of_its_own() {
        let deploy = "\
on:
  push:
    tags: [v*]
jobs:
  ship:
    steps:
      - run: cargo install cargo-release
";
        assert!(verify_from_ci(&files(deploy)).is_empty());
    }

    #[test]
    fn a_command_that_needs_ci_is_left_out() {
        let yaml = "\
on: pull_request
jobs:
  check:
    steps:
      - run: cargo test --target ${{ matrix.target }}
      - run: cargo publish
      - run: kubectl apply -f deploy
      - run: echo done
      - run: cargo test -p core
";
        assert_eq!(verify_from_ci(&files(yaml)), ["cargo test -p core"]);
    }

    #[test]
    fn a_block_that_holds_control_flow_is_left_out() {
        let yaml = "\
on: pull_request
jobs:
  check:
    steps:
      - run: |
          if [ -f Cargo.toml ]; then
            cargo test
          fi
";
        assert!(verify_from_ci(&files(yaml)).is_empty());
    }

    #[test]
    fn a_folded_block_is_left_out_rather_than_split_into_commands() {
        let yaml = "\
on: pull_request
jobs:
  check:
    steps:
      - run: >-
          cargo test
          --workspace
      - run: cargo fmt --check
";
        assert_eq!(verify_from_ci(&files(yaml)), ["cargo fmt --check"]);
    }

    #[test]
    fn one_command_from_two_workflows_is_kept_once_and_the_list_is_bounded() {
        let yaml = "\
on: pull_request
jobs:
  check:
    steps:
      - run: cargo test
      - run: cargo clippy
      - run: cargo fmt --check
      - run: cargo build
      - run: cargo doc
      - run: cargo audit
";
        let mut many = files(yaml);
        many.push(("second.yml".to_owned(), yaml.to_owned()));
        let commands = verify_from_ci(&many);
        assert_eq!(commands.len(), MAX);
        assert_eq!(commands[0], "cargo test");
    }

    #[test]
    fn a_file_that_is_not_a_workflow_is_no_check() {
        assert!(verify_from_ci(&[("ci.txt".to_owned(), CI.to_owned())]).is_empty());
    }

    #[test]
    fn an_inline_comment_does_not_hide_the_checks_after_it() {
        let yaml = "\
on: pull_request
jobs:
  check:
    steps:
      - run: |
          true # setup finished
          test 1 = 2
";
        assert_eq!(verify_from_ci(&files(yaml)), ["true && test 1 = 2"]);
    }

    #[test]
    fn a_status_message_does_not_discard_the_check_after_it() {
        let yaml = "\
on: pull_request
jobs:
  check:
    steps:
      - run: |
          echo \"starting\"
          cargo test
";
        assert_eq!(
            verify_from_ci(&files(yaml)),
            ["echo \"starting\" && cargo test"]
        );
    }
}
