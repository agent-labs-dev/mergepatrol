//! `review-eval`: runs MergeLoop's review on PRs whose bugs are known, and scores it. See
//! `evals/review/README.md`.

mod case;
mod report;
mod run;
mod score;

use std::path::{Path, PathBuf};

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
    Run {
        label: String,
        agent: Agent,
        only: Vec<String>,
        jobs: usize,
        job_bin: Option<PathBuf>,
        out: Option<PathBuf>,
        cases: PathBuf,
    },
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

fn parse(args: &[String]) -> Result<Command> {
    let (mut flags, mut paths) = (Vec::new(), Vec::new());
    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        match arg.strip_prefix("--") {
            Some(name) => {
                let value = it
                    .next()
                    .with_context(|| format!("--{name} takes a value"))?;
                flags.push((name, value.clone()));
            }
            None => paths.push(PathBuf::from(arg)),
        }
    }
    let flag = |name: &str| {
        flags
            .iter()
            .rev()
            .find(|(n, _)| *n == name)
            .map(|f| f.1.clone())
    };
    let cases = flag("cases").map_or(PathBuf::from(CASES), PathBuf::from);
    let known: &[&str] = match args.first().map(String::as_str) {
        Some("run") => &[
            "label", "model", "provider", "effort", "case", "jobs", "job-bin", "out", "cases",
        ],
        _ => &["cases"],
    };
    if let Some((name, _)) = flags.iter().find(|(n, _)| !known.contains(n)) {
        bail!("unknown flag --{name}\n{USAGE}");
    }
    let command = match (args.first().map(String::as_str), paths.as_slice()) {
        (Some("run"), []) => {
            let provider = flag("provider").unwrap_or_else(|| "anthropic".into());
            Command::Run {
                label: flag("label").context("run takes --label")?,
                agent: Agent {
                    provider: Provider::parse(&provider)
                        .with_context(|| format!("{provider:?} is not a provider"))?,
                    model: flag("model").context("run takes --model")?,
                    effort: flag("effort").unwrap_or_else(|| "medium".into()),
                },
                only: flags
                    .iter()
                    .filter(|(n, _)| *n == "case")
                    .map(|f| f.1.clone())
                    .collect(),
                jobs: flag("jobs")
                    .map_or(Ok(2), |j| j.parse())
                    .context("--jobs")?,
                job_bin: flag("job-bin").map(PathBuf::from),
                out: flag("out").map(PathBuf::from),
                cases,
            }
        }
        (Some("report"), [run]) => Command::Report {
            run: run.clone(),
            cases,
        },
        (Some("compare"), [a, b]) => Command::Compare {
            a: a.clone(),
            b: b.clone(),
            cases,
        },
        _ => bail!("{USAGE}"),
    };
    Ok(command)
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

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args)? {
        Command::Run {
            label,
            agent,
            only,
            jobs,
            job_bin,
            out,
            cases,
        } => {
            let mut all = case::load(&cases)?;
            if !only.is_empty() {
                all.retain(|c| only.contains(&c.id));
            }
            let var = key_var(agent.provider);
            let key = std::env::var(var).with_context(|| format!("set {var}"))?;
            let job_bin = match job_bin {
                Some(b) => b,
                // `cargo build -p mergepatrol` puts it next to this binary.
                None => std::env::current_exe()?.with_file_name("mergepatrol"),
            };
            let work = Path::new("target/review-eval").join(&label);
            let out = out.unwrap_or_else(|| work.with_extension("json"));
            let runner = Runner {
                job_bin,
                agent: agent.clone(),
                key,
                jobs,
                work,
            };
            let file = RunFile {
                label,
                agent,
                cases: runner.run(&all).await?,
            };
            std::fs::write(&out, serde_json::to_vec_pretty(&file)?)?;
            print!("{}", report::run(&file.label, &scores(&cases, &file)?));
            eprintln!("wrote {}", out.display());
        }
        Command::Report { run, cases } => {
            let file = read_run(&run)?;
            print!("{}", report::run(&file.label, &scores(&cases, &file)?));
        }
        Command::Compare { a, b, cases } => {
            let (a, b) = (read_run(&a)?, read_run(&b)?);
            let c = score::compare(&scores(&cases, &a)?, &scores(&cases, &b)?);
            print!("{}", report::comparison(&a.label, &b.label, &c));
        }
    }
    Ok(())
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
            Command::Run {
                label: "try".into(),
                agent: Agent {
                    provider: Provider::OpenAi,
                    model: "gpt".into(),
                    effort: "medium".into(),
                },
                only: vec!["a".into(), "b".into()],
                jobs: 4,
                job_bin: None,
                out: Some("o.json".into()),
                cases: CASES.into(),
            }
        );
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
}
