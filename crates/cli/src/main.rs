//! `mergeloop`, the command an agent lands a PR with. `mergeloop [pr]` queues the PR, waits until
//! it needs something of the agent, prints that need with its brief, and exits with a code that
//! says what kind it is. The agent does what the brief says, then runs the command the need gives
//! as `then`. `mergeloop call <tool> [json]` runs any MCP tool of the server.
//!
//! It keeps no state and starts no agent: what a PR needs, and what to do about it, is the
//! server's, through its MCP endpoint, so this stays a thin client.

use std::io::Read;
use std::process::{Command, ExitCode};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

const USAGE: &str = "\
Land a PR through MergeLoop's merge queue.

Usage:
  mergeloop [PR] [--after ID]    Queue the PR, wait until it needs you, print what to do
  mergeloop call TOOL [JSON|-]   Run one of MergeLoop's MCP tools, with JSON arguments

PR is 123, owner/name#123, or a PR's URL. Without one, the current branch's PR.
Do what a need's `brief` says, then run its `then`, until mergeloop exits 10.

Exit codes: 0 a need for you, 10 merged, 11 ejected, 12 waiting on a human, 2 an error.

Environment:
  MERGELOOP_TOKEN   A GitHub token. Without it, `gh auth token`.
  MERGELOOP_URL     The MergeLoop server. Default https://mergeloop.ai.";

/// How long the server waits for a newer need, per call. It allows up to 50.
const WAIT_SECS: u64 = 50;
/// Calls in a row that may fail, as on a dropped connection, before the command gives up.
const RETRIES: u32 = 5;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("mergeloop: {e:#}");
            ExitCode::from(2)
        }
    }
}

#[derive(Debug, PartialEq)]
enum Cmd {
    Land {
        pr: Option<PrRef>,
        after: Option<String>,
    },
    Call {
        tool: String,
        args: Option<String>,
    },
    Help,
    Version,
}

async fn run(args: &[String]) -> Result<u8> {
    match parse(args)? {
        Cmd::Help => {
            println!("{USAGE}");
            Ok(0)
        }
        Cmd::Version => {
            println!("mergeloop {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Cmd::Call { tool, args } => {
            let args = match args.as_deref() {
                None => json!({}),
                Some("-") => {
                    let mut s = String::new();
                    std::io::stdin().read_to_string(&mut s)?;
                    serde_json::from_str(&s).context("the arguments on stdin are not JSON")?
                }
                Some(s) => serde_json::from_str(s).context("the arguments are not JSON")?,
            };
            let (text, is_error) = Client::new()?.tool(&tool, args).await?;
            println!("{text}");
            Ok(if is_error { 1 } else { 0 })
        }
        Cmd::Land { pr, after } => {
            let (repo, number) = resolve(pr)?;
            land(&Client::new()?, &repo, number, after).await
        }
    }
}

fn parse(args: &[String]) -> Result<Cmd> {
    let mut args = args.iter().map(String::as_str);
    let mut pr = None;
    let mut after = None;
    while let Some(arg) = args.next() {
        match arg {
            "-h" | "--help" | "help" => return Ok(Cmd::Help),
            "-V" | "--version" => return Ok(Cmd::Version),
            "call" if pr.is_none() && after.is_none() => {
                let tool = args.next().context("call needs a tool's name")?;
                let rest = args.next().map(str::to_owned);
                if args.next().is_some() {
                    bail!("call takes a tool and one JSON argument");
                }
                return Ok(Cmd::Call {
                    tool: tool.to_owned(),
                    args: rest,
                });
            }
            "--after" => after = Some(args.next().context("--after needs a need's id")?.into()),
            _ if arg.starts_with('-') => bail!("unknown option {arg}\n\n{USAGE}"),
            _ if pr.is_none() => pr = Some(PrRef::parse(arg)?),
            _ => bail!("one PR at a time\n\n{USAGE}"),
        }
    }
    Ok(Cmd::Land { pr, after })
}

/// A PR as the command line names it.
#[derive(Debug, PartialEq)]
struct PrRef {
    /// `owner/name`, or the current directory's repo.
    repo: Option<String>,
    number: u64,
}

impl PrRef {
    fn parse(s: &str) -> Result<Self> {
        let bad = || anyhow::anyhow!("{s} is not a PR: use 123, owner/name#123, or its URL");
        if let Some(path) = s
            .strip_prefix("https://github.com/")
            .or_else(|| s.strip_prefix("github.com/"))
        {
            let parts: Vec<&str> = path.split('/').collect();
            let [owner, name, "pull", number, ..] = parts[..] else {
                return Err(bad());
            };
            return Ok(PrRef {
                repo: Some(format!("{owner}/{name}")),
                number: number.parse().map_err(|_| bad())?,
            });
        }
        let (repo, number) = match s.split_once('#') {
            Some(("", n)) => (None, n),
            Some((repo, n)) if repo.split('/').filter(|p| !p.is_empty()).count() == 2 => {
                (Some(repo.to_owned()), n)
            }
            Some(_) => return Err(bad()),
            None => (None, s),
        };
        Ok(PrRef {
            repo,
            number: number.parse().map_err(|_| bad())?,
        })
    }
}

/// The repo and number, asking `gh` for what the command line left out.
fn resolve(pr: Option<PrRef>) -> Result<(String, u64)> {
    let pr = match pr {
        Some(pr) => pr,
        None => {
            let url = gh(&["pr", "view", "--json", "url", "-q", ".url"])
                .context("no PR given, and `gh` finds none for the current branch")?;
            PrRef::parse(&url)?
        }
    };
    let repo = match pr.repo {
        Some(repo) => repo,
        None => gh(&[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ])
        .context("no repo given, and `gh` finds none for the current directory")?,
    };
    Ok((repo, pr.number))
}

fn gh(args: &[&str]) -> Result<String> {
    let out = Command::new("gh")
        .args(args)
        .output()
        .context("run `gh`, the GitHub CLI")?;
    if !out.status.success() {
        bail!(
            "`gh {}`: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// Queues the PR, and waits until it needs the agent: prints that need, and returns its exit
/// code.
async fn land(client: &Client, repo: &str, number: u64, mut after: Option<String>) -> Result<u8> {
    let mut said = String::new();
    loop {
        let mut args = json!({ "repo": repo, "number": number, "wait": WAIT_SECS });
        if let Some(after) = &after {
            args["after"] = after.as_str().into();
        }
        let need = client.json("next_need", args).await?;
        let id = need["id"].as_str().unwrap_or_default().to_owned();
        match need["needs"].as_str() {
            Some("wait") => {
                // For whoever watches the terminal, once per change.
                let brief = need["brief"].as_str().unwrap_or_default();
                if brief != said {
                    eprintln!("mergeloop: {brief}");
                    said = brief.to_owned();
                }
            }
            Some("queue") => {
                if after.as_deref() != Some(id.as_str()) {
                    client
                        .json("queue_pull", json!({ "repo": repo, "number": number }))
                        .await?;
                    eprintln!("mergeloop: queued {repo}#{number}");
                }
                after = Some(id);
            }
            Some(_) => {
                println!("{}", serde_json::to_string_pretty(&printed(&need))?);
                let code = need["exit_code"].as_u64().unwrap_or(2);
                return Ok(u8::try_from(code).unwrap_or(2));
            }
            None => bail!("the server answered no need: {need}"),
        }
    }
}

/// The need as the agent reads it: what to do, without the whole status.
fn printed(need: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for field in ["needs", "repo", "number", "layer", "brief", "then", "id"] {
        if !need[field].is_null() {
            out.insert(field.into(), need[field].clone());
        }
    }
    Value::Object(out)
}

/// MergeLoop's MCP endpoint, called as the GitHub token's user.
struct Client {
    http: reqwest::Client,
    url: String,
    token: String,
}

impl Client {
    fn new() -> Result<Self> {
        let token = match std::env::var("MERGELOOP_TOKEN") {
            Ok(t) if !t.is_empty() => t,
            _ => gh(&["auth", "token"])
                .context("no MERGELOOP_TOKEN, and `gh auth token` has none: run `gh auth login`")?,
        };
        let url = std::env::var("MERGELOOP_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "https://mergeloop.ai".into());
        let http = reqwest::Client::builder()
            // The server waits up to `WAIT_SECS`.
            .timeout(Duration::from_secs(WAIT_SECS + 40))
            .user_agent(concat!("mergeloop-cli/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Client {
            http,
            url: url.trim_end_matches('/').to_owned(),
            token,
        })
    }

    /// A tool's answer, parsed: its error is the command's.
    async fn json(&self, tool: &str, args: Value) -> Result<Value> {
        let (text, is_error) = self.tool(tool, args).await?;
        if is_error {
            bail!("{tool}: {text}");
        }
        serde_json::from_str(&text).with_context(|| format!("{tool} answered no JSON: {text}"))
    }

    /// A tool's text, and whether it is an error, retrying what failed on the way.
    async fn tool(&self, tool: &str, args: Value) -> Result<(String, bool)> {
        let mut failed = 0;
        loop {
            match self.call(tool, &args).await {
                Ok(answer) => return Ok(answer),
                Err(Retry::No(e)) => return Err(e),
                Err(Retry::Yes(e)) if failed + 1 >= RETRIES => return Err(e),
                Err(Retry::Yes(e)) => {
                    failed += 1;
                    eprintln!("mergeloop: {e:#}; trying again");
                    tokio::time::sleep(Duration::from_secs(5 * u64::from(failed))).await;
                }
            }
        }
    }

    async fn call(&self, tool: &str, args: &Value) -> Result<(String, bool), Retry> {
        let message = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        });
        let res = self
            .http
            .post(format!("{}/mcp", self.url))
            .bearer_auth(&self.token)
            .json(&message)
            .send()
            .await
            .map_err(|e| {
                Retry::Yes(anyhow::Error::new(e).context(format!("reach {}", self.url)))
            })?;
        let status = res.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Retry::No(anyhow::anyhow!(
                "{} does not take the GitHub token: check MERGELOOP_TOKEN or `gh auth status`",
                self.url
            )));
        }
        let body = res.text().await.map_err(|e| Retry::Yes(e.into()))?;
        if status.is_server_error() {
            return Err(Retry::Yes(anyhow::anyhow!(
                "{} answered {status}",
                self.url
            )));
        }
        if !status.is_success() {
            return Err(Retry::No(anyhow::anyhow!(
                "{} answered {status}: {body}",
                self.url
            )));
        }
        let answer: Value = serde_json::from_str(&body)
            .map_err(|_| Retry::No(anyhow::anyhow!("{} answered no JSON: {body}", self.url)))?;
        if let Some(error) = answer.get("error") {
            return Err(Retry::No(anyhow::anyhow!("{}", error["message"])));
        }
        let result = &answer["result"];
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        Ok((text.to_owned(), result["isError"] == true))
    }
}

/// Whether a failed call is worth making again.
enum Retry {
    Yes(anyhow::Error),
    No(anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn a_pr_is_a_number_a_repos_number_or_a_url() {
        let pr = |repo: Option<&str>, number| PrRef {
            repo: repo.map(str::to_owned),
            number,
        };
        assert_eq!(PrRef::parse("123").unwrap(), pr(None, 123));
        assert_eq!(PrRef::parse("#123").unwrap(), pr(None, 123));
        assert_eq!(PrRef::parse("o/r#5").unwrap(), pr(Some("o/r"), 5));
        assert_eq!(
            PrRef::parse("https://github.com/o/r/pull/5/files").unwrap(),
            pr(Some("o/r"), 5)
        );
        for bad in [
            "o#5",
            "o/r/x#5",
            "r#x",
            "https://github.com/o/r/issues/5",
            "",
        ] {
            assert!(PrRef::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_command_is_the_loop_or_a_call() {
        assert_eq!(
            parse(&args("")).unwrap(),
            Cmd::Land {
                pr: None,
                after: None
            }
        );
        assert_eq!(
            parse(&args("o/r#5 --after abc")).unwrap(),
            Cmd::Land {
                pr: Some(PrRef {
                    repo: Some("o/r".into()),
                    number: 5
                }),
                after: Some("abc".into()),
            }
        );
        assert_eq!(
            parse(&args("call pull_status {}")).unwrap(),
            Cmd::Call {
                tool: "pull_status".into(),
                args: Some("{}".into())
            }
        );
        assert_eq!(parse(&args("--help")).unwrap(), Cmd::Help);
        assert!(parse(&args("5 6")).is_err());
        assert!(parse(&args("--bogus")).is_err());
        assert!(parse(&args("call")).is_err());
    }

    #[test]
    fn the_printed_need_leaves_out_the_status() {
        let need = json!({
            "needs": "threads", "id": "x", "repo": "o/r", "number": 5, "layer": null,
            "brief": "b", "then": "t", "exit_code": 0, "status": {},
        });
        assert_eq!(
            printed(&need),
            json!({ "needs": "threads", "id": "x", "repo": "o/r", "number": 5, "brief": "b", "then": "t" })
        );
    }
}
