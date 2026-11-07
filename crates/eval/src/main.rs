//! `review-eval`: runs MergeLoop's review on PRs whose bugs are known, and scores it. See
//! `evals/review/README.md`.

mod case;
mod report;
mod run;
mod score;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use mergepatrol_core::{Provider, ReviewSettings};

use crate::run::{Agent, RunFile, Runner};

const USAGE: &str = "usage:
  review-eval run --label <name> --model <model> [--provider anthropic|openai|openrouter]
                  [--effort medium] [--case <id>]... [--jobs 2] [--job-bin <mergepatrol>]
                  [--out <run.json>] [--cases <dir>]
  review-eval report <run.json> [--cases <dir>]
  review-eval compare <a.json> <b.json> [--cases <dir>]";

const CASES: &str = "evals/review/cases";

#[derive(Debug, PartialEq)]
enum Command {
    Run(RunArgs),
    Report {
        run: PathBuf,
        cases: PathBuf,
    },
    Compare {
        a: PathBuf,
        b: PathBuf,
        cases: PathBuf,
    },
}

#[derive(Debug, PartialEq)]
struct RunArgs {
    label: String,
    agent: Agent,
    /// The cases to run, by id: all of them when empty.
    only: Vec<String>,
    jobs: usize,
    /// The `mergepatrol` next to this binary, unless `--job-bin` names one.
    job_bin: PathBuf,
    /// `target/review-eval/<label>.json`, unless `--out` names a file.
    out: PathBuf,
    cases: PathBuf,
}

/// The `--name value` flags of a command line, and the rest.
struct Flags {
    flags: Vec<(String, String)>,
    paths: Vec<PathBuf>,
}

impl Flags {
    fn read(args: &[String], known: &[&str]) -> Result<Flags> {
        let (mut flags, mut paths) = (Vec::new(), Vec::new());
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let Some(name) = arg.strip_prefix("--") else {
                paths.push(PathBuf::from(arg));
                continue;
            };
            if !known.contains(&name) {
                bail!("unknown flag --{name}\n{USAGE}");
            }
            let value = it
                .next()
                .with_context(|| format!("--{name} takes a value"))?;
            flags.push((name.to_owned(), value.clone()));
        }
        Ok(Flags { flags, paths })
    }

    /// Every value of a flag, in order.
    fn all(&self, name: &str) -> Vec<String> {
        self.flags
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .collect()
    }

    /// A flag's last value.
    fn get(&self, name: &str) -> Option<String> {
        self.all(name).pop()
    }

    fn cases(&self) -> PathBuf {
        self.get("cases")
            .map_or(PathBuf::from(CASES), PathBuf::from)
    }
}

const RUN_FLAGS: &[&str] = &[
    "label", "model", "provider", "effort", "case", "jobs", "job-bin", "out", "cases",
];

fn parse(args: &[String]) -> Result<Command> {
    let (name, rest) = args.split_first().context(USAGE)?;
    let known = if name == "run" { RUN_FLAGS } else { &["cases"] };
    let f = Flags::read(rest, known)?;
    match (name.as_str(), f.paths.as_slice()) {
        ("run", []) => Ok(Command::Run(run_args(&f)?)),
        ("report", [run]) => Ok(Command::Report {
            run: run.clone(),
            cases: f.cases(),
        }),
        ("compare", [a, b]) => Ok(Command::Compare {
            a: a.clone(),
            b: b.clone(),
            cases: f.cases(),
        }),
        _ => bail!("{USAGE}"),
    }
}

fn run_args(f: &Flags) -> Result<RunArgs> {
    let provider = f.get("provider").unwrap_or_else(|| "anthropic".into());
    let jobs = f.get("jobs").unwrap_or_else(|| "2".into());
    let label = f.get("label").context("run takes --label")?;
    Ok(RunArgs {
        agent: Agent {
            provider: Provider::parse(&provider)
                .with_context(|| format!("{provider:?} is not a provider"))?,
            model: f.get("model").context("run takes --model")?,
            effort: f.get("effort").unwrap_or_else(|| "medium".into()),
        },
        only: f.all("case"),
        jobs: jobs.parse().with_context(|| format!("--jobs {jobs:?}"))?,
        // `cargo build -p mergepatrol` puts it next to this binary.
        job_bin: match f.get("job-bin") {
            Some(bin) => bin.into(),
            None => std::env::current_exe()?.with_file_name("mergepatrol"),
        },
        out: f.get("out").map_or_else(
            || Path::new("target/review-eval").join(format!("{label}.json")),
            PathBuf::from,
        ),
        cases: f.cases(),
        label,
    })
}

/// The environment variable that holds a provider's API key.
fn key_var(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "ANTHROPIC_API_KEY",
        Provider::OpenAi => "OPENAI_API_KEY",
        Provider::OpenRouter => "OPENROUTER_API_KEY",
    }
}

fn read_run(path: &Path) -> Result<RunFile> {
    let json = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&json).with_context(|| format!("{} is not a run", path.display()))
}

fn scores(cases: &Path, run: &RunFile) -> Result<Vec<score::Scored>> {
    let cases = case::load(cases)?;
    Ok(score::scores(
        &cases,
        &run.cases,
        &ReviewSettings::default(),
    ))
}

/// Runs a command, and returns what it prints. `key` reads an environment variable.
async fn execute(command: Command, key: impl Fn(&str) -> Option<String>) -> Result<String> {
    match command {
        Command::Run(args) => {
            let var = key_var(args.agent.provider);
            let key = key(var).with_context(|| format!("set {var}"))?;
            run(args, key).await
        }
        Command::Report { run, cases } => report_run(&run, &cases),
        Command::Compare { a, b, cases } => compare_runs(&a, &b, &cases),
    }
}

fn report_run(run: &Path, cases: &Path) -> Result<String> {
    let file = read_run(run)?;
    Ok(report::run(&file.label, &scores(cases, &file)?))
}

fn compare_runs(a: &Path, b: &Path, cases: &Path) -> Result<String> {
    let (a, b) = (read_run(a)?, read_run(b)?);
    let c = score::compare(&scores(cases, &a)?, &scores(cases, &b)?);
    Ok(report::comparison(&a.label, &b.label, &c))
}

/// Reviews the cases, writes the run, and scores it.
async fn run(args: RunArgs, key: String) -> Result<String> {
    let mut cases = case::load(&args.cases)?;
    cases.retain(|c| args.only.is_empty() || args.only.contains(&c.id));
    let out = &args.out;
    let runner = Runner {
        job_bin: args.job_bin.clone(),
        agent: args.agent.clone(),
        key,
        jobs: args.jobs,
        // Each case's job output, next to the run.
        work: out.with_extension(""),
    };
    let file = RunFile {
        label: args.label,
        agent: args.agent,
        cases: runner.run(&cases).await?,
    };
    std::fs::write(out, serde_json::to_vec_pretty(&file)?)
        .with_context(|| format!("write {}", out.display()))?;
    eprintln!("wrote {}", out.display());
    Ok(report::run(&file.label, &scores(&args.cases, &file)?))
}

/// The command line's command, run.
async fn cli(args: &[String], key: impl Fn(&str) -> Option<String>) -> Result<String> {
    execute(parse(args)?, key).await
}

/// Prints what a command printed, or why it failed, and exits to match.
fn finish(
    ran: Result<String>,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> ExitCode {
    let (text, sink, code) = match ran {
        Ok(text) => (text, out as &mut dyn std::io::Write, ExitCode::SUCCESS),
        Err(e) => (
            format!("{e:#}\n"),
            err as &mut dyn std::io::Write,
            ExitCode::FAILURE,
        ),
    };
    let _ = sink.write_all(text.as_bytes());
    code
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ran = cli(&args, |var| std::env::var(var).ok()).await;
    finish(ran, &mut std::io::stdout(), &mut std::io::stderr())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn run_reads_its_agent_and_which_cases() {
        let c = parse(&args(
            "run --label try --model gpt --provider openai --case a --case b --jobs 4 --out o.json",
        ))
        .unwrap();
        assert_eq!(
            c,
            Command::Run(RunArgs {
                label: "try".into(),
                agent: Agent {
                    provider: Provider::OpenAi,
                    model: "gpt".into(),
                    effort: "medium".into(),
                },
                only: vec!["a".into(), "b".into()],
                jobs: 4,
                job_bin: std::env::current_exe()
                    .unwrap()
                    .with_file_name("mergepatrol"),
                out: "o.json".into(),
                cases: CASES.into(),
            })
        );
    }

    #[test]
    fn a_run_is_written_under_target_by_its_label() {
        let Command::Run(a) = parse(&args("run --label base --model m")).unwrap() else {
            panic!("not a run");
        };
        assert_eq!(a.out, Path::new("target/review-eval/base.json"));
        assert_eq!((a.jobs, a.agent.provider), (2, Provider::Anthropic));
        assert!(a.only.is_empty());
    }

    #[test]
    fn report_and_compare_take_run_files() {
        assert_eq!(
            parse(&args("report r.json --cases d")).unwrap(),
            Command::Report {
                run: "r.json".into(),
                cases: "d".into()
            }
        );
        assert_eq!(
            parse(&args("compare a.json b.json")).unwrap(),
            Command::Compare {
                a: "a.json".into(),
                b: "b.json".into(),
                cases: CASES.into()
            }
        );
    }

    #[test]
    fn a_command_it_cannot_read_is_an_error() {
        for bad in [
            "",
            "run --model m",
            "run --label l",
            "run --label l --model m --provider x",
            "run --label l --model m --jobs x",
            "run --label l --model m --nope 1",
            "run --label l --model m extra",
            "report",
            "report r.json --model m",
            "compare a.json",
            "run --label",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn each_provider_reads_its_own_key() {
        let vars: Vec<_> = [Provider::Anthropic, Provider::OpenAi, Provider::OpenRouter]
            .map(key_var)
            .into();
        assert_eq!(
            vars,
            ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY"]
        );
    }

    fn write_case(dir: &Path, pr: u64, expected: &str) {
        let yaml = format!(
            "repo: o/r\npr: {pr}\nbase: {}\nhead: {}\ntitle: T\nexpected:\n{expected}",
            "a".repeat(40),
            "b".repeat(40)
        );
        std::fs::write(dir.join(format!("pr-{pr}.yml")), yaml).unwrap();
    }

    #[tokio::test]
    async fn run_reviews_the_cases_writes_the_run_and_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        let cases = dir.path().join("cases");
        std::fs::create_dir(&cases).unwrap();
        write_case(&cases, 1, "  clean\n");
        write_case(&cases, 2, "  clean\n");
        let out = dir.path().join("run.json");
        let command = |only: &str| {
            parse(&args(&format!(
                "run --label L --model m --case {only} --out {} --cases {} --job-bin {}",
                out.display(),
                cases.display(),
                run::fake::job(dir.path()).display()
            )))
            .unwrap()
        };
        let key = |var: &str| (var == "ANTHROPIC_API_KEY").then(|| "k".to_owned());
        let text = execute(command("pr-1"), key).await.unwrap();
        assert!(text.starts_with("L\n"), "{text}");
        assert!(text.contains("pr-1") && !text.contains("pr-2"), "{text}");
        assert!(text.contains("$0.25 over 1 cases"), "{text}");
        assert_eq!(read_run(&out).unwrap().cases.len(), 1);
        assert!(dir.path().join("run/pr-1/result.json").exists());

        let no_key = execute(command("pr-1"), |_| None).await.unwrap_err();
        assert_eq!(no_key.to_string(), "set ANTHROPIC_API_KEY");

        let report = format!("report {} --cases {}", out.display(), cases.display());
        assert_eq!(cli(&args(&report), key).await.unwrap(), text);

        let compare = format!(
            "compare {} {} --cases {}",
            out.display(),
            out.display(),
            cases.display()
        );
        let text = cli(&args(&compare), key).await.unwrap();
        assert!(
            text.starts_with("A L: ") && text.ends_with("verdict: drop B\n"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_run_file_that_is_missing_or_not_a_run_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{}").unwrap();
        for path in [bad, dir.path().join("gone.json")] {
            let c = parse(&args(&format!("report {}", path.display()))).unwrap();
            assert!(execute(c, |_| None).await.is_err());
        }
    }

    #[test]
    fn a_command_prints_what_it_ran_or_why_it_failed() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(
            finish(Ok("done\n".into()), &mut out, &mut err),
            ExitCode::SUCCESS
        );
        assert_eq!((out.as_slice(), err.as_slice()), (&b"done\n"[..], &b""[..]));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let failed = finish(Err(anyhow::anyhow!("no key")), &mut out, &mut err);
        assert_eq!(failed, ExitCode::FAILURE);
        assert_eq!(
            (out.as_slice(), err.as_slice()),
            (&b""[..], &b"no key\n"[..])
        );
    }
}
