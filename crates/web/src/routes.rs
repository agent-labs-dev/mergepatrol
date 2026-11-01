//! Every route the server answers, and the MCP tool that calls it. The dashboard does nothing
//! but call these routes, so a tool for each is what lets an agent do all the dashboard does. A
//! route no agent should call says why.
//!
//! A tool calls its route in this process, as the agent: the route checks who may, and its
//! answer is the tool's. The route's path parameters are the tool's arguments, as `PATH_ARGS`
//! names them, and so are the `args`: the query of a GET, the JSON body of anything else.

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::handler::Handler;
use axum::http::Method;
use axum::routing::{MethodFilter, MethodRouter, on};
use serde_json::{Map, Value, json};

use crate::{AppState, api, auth, billing, changelog, mcp, oauth, review, v1, webhook};

pub(crate) struct Route {
    pub method: Method,
    pub path: &'static str,
    handler: fn() -> MethodRouter<AppState>,
    pub agent: Agent,
}

pub(crate) enum Agent {
    Tool(Tool),
    /// Why no agent calls it.
    Exempt(
        #[cfg_attr(not(test), expect(dead_code, reason = "for whoever reads the table"))]
        &'static str,
    ),
}

pub(crate) struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub args: &'static [Arg],
    pub effect: Effect,
}

pub(crate) enum Effect {
    Reads,
    Writes,
    /// Undoes something the user set up, as removing a key does.
    Destroys,
}

pub(crate) struct Arg {
    pub name: &'static str,
    pub ty: Ty,
    pub description: &'static str,
    pub required: bool,
}

pub(crate) enum Ty {
    Int,
    Str,
    Bool,
    Strs,
    OneOf(&'static [&'static str]),
    /// An object that is the whole JSON body.
    Body,
}

/// A path parameter, the argument it is, and the argument's schema. `{owner}/{name}` is one
/// argument: `repo`.
pub(crate) static PATH_ARGS: [(&str, &str, Ty, &str); 5] = [
    (
        "id",
        "workspace",
        Ty::Int,
        "The workspace: the installation ID that `get_session` lists. A tool with a `repo` finds it from the repo without one.",
    ),
    ("owner", "repo", Ty::Str, "The repo, as `owner/name`."),
    ("number", "number", Ty::Int, "The PR's number."),
    (
        "provider",
        "provider",
        Ty::Str,
        "The model provider, as `get_settings` lists them.",
    ),
    (
        "login",
        "login",
        Ty::Str,
        "The GitHub login the seat is for.",
    ),
];

const fn arg(name: &'static str, ty: Ty, description: &'static str) -> Arg {
    Arg {
        name,
        ty,
        description,
        required: true,
    }
}

const fn opt(name: &'static str, ty: Ty, description: &'static str) -> Arg {
    Arg {
        name,
        ty,
        description,
        required: false,
    }
}

const SIGN_IN: &str =
    "Signing in is GitHub's OAuth, in a browser. An MCP client signs in through `/oauth`.";
const SETUP: &str = "Creating the GitHub App is GitHub's manifest flow, in a browser.";
const OAUTH: &str = "MCP's OAuth sign-in.";

macro_rules! route {
    ($method:ident $path:literal, $handler:expr, $agent:expr) => {
        Route {
            method: Method::$method,
            path: $path,
            handler: || on(MethodFilter::$method, $handler),
            agent: $agent,
        }
    };
}

macro_rules! tool {
    ($name:literal, $effect:ident, $description:literal $(, $arg:expr)* $(,)?) => {
        Agent::Tool(Tool {
            name: $name,
            description: $description,
            args: &[$($arg),*],
            effect: Effect::$effect,
        })
    };
}

pub(crate) static ROUTES: &[Route] = &[
    route!(GET "/api/health", || async { "ok" }, Agent::Exempt("A liveness probe.")),
    route!(GET "/api/changelog", changelog::latest, tool!("get_changelog", Reads,
        "Mergepatrol's newest changelog entries, newest first: what changed for its users, each as a slug, a title, and new, improved or fixed. Each entry's page is /changelog/<slug>.")),
    route!(GET "/api/session", auth::session, tool!("get_session", Reads,
        "Who you are signed in to Mergepatrol as, and your workspaces: the installations of its GitHub App you can reach. A GitHub token is signed in as no one, and has no workspaces: it reaches single repos.")),
    route!(POST "/api/setup/unlock", auth::unlock, Agent::Exempt(SETUP)),
    route!(POST "/api/setup/manifest", auth::start_manifest, Agent::Exempt(SETUP)),
    route!(GET "/setup/callback", auth::manifest_callback, Agent::Exempt(SETUP)),
    route!(GET "/setup/installed", auth::installed, Agent::Exempt(SETUP)),
    route!(GET "/auth/login", auth::login, Agent::Exempt(SIGN_IN)),
    route!(GET "/auth/callback", auth::login_callback, Agent::Exempt(SIGN_IN)),
    route!(POST "/auth/logout", auth::logout, Agent::Exempt(SIGN_IN)),
    route!(POST "/auth/tray/token", auth::tray_token, Agent::Exempt("The menu bar app's sign-in.")),
    route!(POST "/webhooks/github",
        webhook::receive.layer(DefaultBodyLimit::max(webhook::MAX_BODY_BYTES)),
        Agent::Exempt("GitHub calls it.")),
    route!(POST "/webhooks/stripe", billing::webhook, Agent::Exempt("Stripe calls it.")),
    route!(POST "/mcp", mcp::post, Agent::Exempt("The MCP server itself.")),
    route!(GET "/mcp", mcp::get, Agent::Exempt("The MCP server itself.")),
    route!(GET "/.well-known/oauth-protected-resource", oauth::resource, Agent::Exempt(OAUTH)),
    route!(GET "/.well-known/oauth-protected-resource/mcp", oauth::resource, Agent::Exempt(OAUTH)),
    route!(GET "/.well-known/oauth-authorization-server", oauth::server, Agent::Exempt(OAUTH)),
    route!(POST "/oauth/register", oauth::register, Agent::Exempt(OAUTH)),
    route!(GET "/oauth/authorize", oauth::authorize, Agent::Exempt(OAUTH)),
    route!(POST "/oauth/token", oauth::token, Agent::Exempt(OAUTH)),
    route!(GET "/api/v1/repos/{owner}/{name}/pulls/{number}", v1::pull,
        Agent::Exempt("For other apps, by repo and with an ETag. An agent calls `pull_status`, which says the same where Mergepatrol is installed.")),
    route!(GET "/api/v1/repos/{owner}/{name}/pulls", v1::pulls,
        Agent::Exempt("For other apps, by repo and with an ETag. An agent calls `pull_status`, which says the same where Mergepatrol is installed.")),
    route!(GET "/api/you", api::you, tool!("get_inbox", Reads,
        "Your PRs, the PRs that ask for your review, and your PRs merged in the last week, across your workspaces, with where the queue has each, and until when you snoozed it. Needs a Mergepatrol sign-in.")),
    route!(GET "/api/w/{id}/lanes", api::lanes, tool!("list_queue", Reads,
        "The queues, one per repo and branch PRs merge into, each in the order it takes them. Without a `repo`, the workspace's: that needs a Mergepatrol sign-in.",
        opt("repo", Ty::Str, "Only this repo's queues, as `owner/name`."))),
    route!(GET "/api/w/{id}/events", api::events, tool!("list_events", Reads,
        "A repo's queue events, newest first, up to 200: what the queue did, and what its agent spent.",
        arg("repo", Ty::Str, "The repo, as `owner/name`."),
        opt("pr", Ty::Int, "Only this PR's events."))),
    route!(GET "/api/w/{id}/fix", api::fix, tool!("get_fix", Reads,
        "The diff of a commit the fix agent pushed, as an event names it. No other commit's.",
        arg("repo", Ty::Str, "The repo, as `owner/name`."),
        arg("sha", Ty::Str, "The fix commit's full SHA."))),
    route!(GET "/api/w/{id}/history", api::history, tool!("list_history", Reads,
        "The PRs that left the queue, newest first, 50 a page: merged, ejected, or removed. Needs a Mergepatrol sign-in.",
        arg("days", Ty::Int, "How many days back, from 1 to 365."),
        opt("repo", Ty::Str, "Only this repo's, as `owner/name`."),
        opt("author", Ty::Str, "Only this GitHub login's."),
        opt("outcome", Ty::OneOf(&["merged", "ejected", "removed"]), "Only the PRs that left this way."),
        opt("before", Ty::Int, "The `next` of the page before."))),
    route!(GET "/api/w/{id}/history/stats", api::history_stats, tool!("get_history_stats", Reads,
        "The queue's numbers over a window and the window before: PRs merged, ejected and removed, time in the queue, time in each step, each developer's, and what the agents spent. Needs a Mergepatrol sign-in.",
        arg("days", Ty::Int, "How many days back, from 1 to 365."),
        opt("repo", Ty::Str, "Only this repo's, as `owner/name`."),
        opt("author", Ty::Str, "Only this GitHub login's."))),
    route!(GET "/api/w/{id}/repos", api::repos, tool!("list_repos", Reads,
        "The repos the workspace's installation reaches, and whether each queues: by its config file, or turned on or off. Needs a Mergepatrol sign-in.")),
    route!(PUT "/api/w/{id}/repos/{owner}/{name}", api::set_repo, tool!("set_repo", Writes,
        "Turns a repo's queue on or off, over what its config file says, or back to following the file. Needs a Mergepatrol sign-in.",
        opt("enabled", Ty::Bool, "`true` on, `false` off. Without it, the repo follows its config file."))),
    route!(GET "/api/w/{id}/repos/{owner}/{name}/open", api::open_pulls, tool!("list_open_pulls", Reads,
        "The repo's open PRs into its default branch that are not queued, last updated first, up to 50.")),
    route!(GET "/api/w/{id}/repos/{owner}/{name}/pulls/{number}", api::pull_detail, tool!("get_pull", Reads,
        "An open PR's description, labels, checks, and timeline.")),
    route!(GET "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/diff", api::pull_diff, tool!("get_pull_diff", Reads,
        "A PR's changes, or one of its commits', as `git diff` prints them.",
        opt("commit", Ty::Str, "One of the PR's commits, by full SHA. The whole PR without one."))),
    route!(GET "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/status", api::pull_status, tool!("pull_status", Reads,
        "Where a PR is, as `GET /api/v1/repos/{owner}/{name}/pulls/{number}` says it: whether the repo's queue runs (`mergepatrol`), the PR's place in the queue (`queue`, null when it is not queued, with a typed `hold` while held), how it last left the queue (`merged` or `ejected`), its dashboard link (`url`), and its latest queue events, newest first.")),
    route!(PUT "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/queue", api::queue_pull, tool!("queue_pull", Writes,
        "Queue a PR to merge: Mergepatrol rebases it, tests it, and merges it once its checks pass. On a queued PR, changes its priority. Queuing one PR of a stack queues the whole stack. Needs triage access to the repo.",
        opt("priority", Ty::Bool, "Put it at the front of the queue. Default false."))),
    route!(DELETE "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/queue", api::dequeue_pull, tool!("dequeue_pull", Writes,
        "Take a PR, and its stack if it is in one, out of the queue. Cancels its run. Needs triage access to the repo.")),
    route!(POST "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/close", api::close_pull, tool!("close_pull", Destroys,
        "Closes an open PR.")),
    route!(POST "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/review", api::review_pull, tool!("review_pull", Writes,
        "Reviews a PR: approves it, asks for changes, or comments. Then queues it, if you ask. GitHub takes no approval from a PR's author: theirs posts its text as a comment, and is refused when GitHub requires an approval to merge.",
        arg("verdict", Ty::OneOf(&["approve", "request_changes", "comment"]), "What the review says."),
        arg("body", Ty::Str, "The review's text. May be empty for an approval."),
        arg("queue", Ty::Bool, "Queue the PR after the review."))),
    route!(GET "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/agent-review", review::agent_review, tool!("get_agent_review", Reads,
        "The agent's review of a queued PR, the queue's first gate: its newest, of whatever head, or null before its first. Each finding says what the agent saw, how sure it was, and where it went: back to the PR's author (`sent_back`), to a human (`asked`, for a call the author cannot make from the PR, `needs_human`), withdrawn on the author's reply (`withdrawn`), or cleared. A finding that holds the PR has a review thread on its line (`threaded`): the author fixes it or replies why not, then resolves the thread. `status` is `changes` while it waits for the author, `asks` while it waits for a human's answers, and `approved` once the author resolved every thread, or nothing held the PR.")),
    route!(POST "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/agent-review/answers", review::answer_agent_review, tool!("answer_agent_review", Writes,
        "Answers what the agent's review asks: posts your review of the PR on GitHub, with each change you ask for on its line. All fine approves it, and it takes its place in the queue (the PR's author's approval goes as a comment, and is refused when GitHub requires an approval to merge); a change holds it for the author's next push (asked by the author, it goes as a comment, as GitHub takes no request for changes from them). Needs write access to the repo, as a user: a workflow's token names none.",
        arg("answers", Ty::Body, "`{ head_sha, answers, note, request_changes }`: the head the review is of, as `get_agent_review` returns it; one answer per finding it asks about, `{ finding, call: \"fine\" | \"change\", note }`, with what to change in `note`; optionally a note for the author; and `request_changes: true` to ask for changes to a PR the agent could not review, which asks about no finding."))),
    route!(POST "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/agent-review/questions", review::ask_agent, tool!("ask_agent_about_review", Writes,
        "Asks the agent a question about one finding of its review, while the review asks a human; or, for a finding sent back to the PR's author (`sent_back`), replies to it, as when the author thinks it is wrong. The agent reads the code again to answer: the answer shows in `get_agent_review`'s `questions` in a minute or two. On a reply it ends `VERDICT: withdrawn` or `VERDICT: stands`; a withdrawn finding no longer holds the PR. Needs write access to the repo.",
        arg("head_sha", Ty::Str, "The head the review is of."),
        arg("finding", Ty::Str, "The finding's id."),
        arg("question", Ty::Str, "What to ask."))),
    route!(POST "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/agent-review/reviewers", review::ask_reviewers, tool!("ask_agent_review_reviewers", Writes,
        "Asks others to answer the agent's review, in place of those it asked: requests their reviews on GitHub, as you.",
        arg("reviewers", Ty::Strs, "Their GitHub logins."))),
    route!(PUT "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/seen", api::seen_pull, tool!("mark_pull_seen", Writes,
        "Marks what was said on a PR so far as read, in your inbox. Needs a Mergepatrol sign-in.")),
    route!(PUT "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/snooze", api::snooze_pull, tool!("snooze_pull", Writes,
        "Puts a PR out of your inbox for a while: `get_inbox` gives it a `snoozed_until`, and the dashboard lists it under Snoozed. It comes back when the time is up, or sooner once someone else comments or reviews. Needs a Mergepatrol sign-in.",
        arg("minutes", Ty::Int, "How long, from 1 to 527040, a year's."))),
    route!(DELETE "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/snooze", api::unsnooze_pull, tool!("unsnooze_pull", Writes,
        "Puts a snoozed PR back in your inbox now. Needs a Mergepatrol sign-in.")),
    route!(POST "/api/w/{id}/repos/{owner}/{name}/pulls/{number}/nudge", api::nudge_pull, tool!("nudge_reviewers", Writes,
        "Asks reviewers to review a PR, in a comment that mentions them, so GitHub tells them.",
        arg("reviewers", Ty::Strs, "Their GitHub logins."))),
    route!(GET "/api/w/{id}/repos/{owner}/{name}/config", api::repo_config, tool!("get_repo_config", Reads,
        "A repo's queue settings, as its config file on the default branch sets them, or as the open settings PR would. With the defaults, and the models and efforts to pick from.")),
    route!(PUT "/api/w/{id}/repos/{owner}/{name}/config", api::propose_repo_config, tool!("propose_repo_config", Writes,
        "Opens a PR that writes these settings to the repo's config file, or updates the one open. The queue uses them once it merges. Needs a Mergepatrol sign-in.",
        arg("settings", Ty::Body, "Every setting, as `get_repo_config` returns them in `settings`."))),
    route!(GET "/api/w/{id}/settings", api::settings, tool!("get_settings", Reads,
        "The workspace's model API keys and ChatGPT login: which are set, and when. Never the keys themselves. Needs a Mergepatrol sign-in.")),
    route!(PUT "/api/w/{id}/settings/api-keys/{provider}", api::set_api_key, tool!("set_api_key", Writes,
        "Sets the workspace's API key for a model provider, once the provider takes it. Needs a Mergepatrol sign-in.",
        arg("value", Ty::Str, "The key."))),
    route!(DELETE "/api/w/{id}/settings/api-keys/{provider}", api::delete_api_key, tool!("delete_api_key", Destroys,
        "Removes the workspace's API key for a model provider. Needs a Mergepatrol sign-in.")),
    route!(POST "/api/w/{id}/settings/chatgpt", api::start_chatgpt_login, tool!("start_chatgpt_login", Writes,
        "Starts a sign-in to a ChatGPT plan, for the workspace's Codex runs: returns a code, and the page where the user enters it. Then call `poll_chatgpt_login`. Needs a Mergepatrol sign-in.")),
    route!(DELETE "/api/w/{id}/settings/chatgpt", api::delete_chatgpt_login, tool!("delete_chatgpt_login", Destroys,
        "Removes the workspace's ChatGPT login. Needs a Mergepatrol sign-in.")),
    route!(POST "/api/w/{id}/settings/chatgpt/poll", api::poll_chatgpt_login, tool!("poll_chatgpt_login", Writes,
        "Asks once whether the user entered the ChatGPT code, and keeps the login once they have. Call it no more often than the sign-in's `interval_secs`. Needs a Mergepatrol sign-in.")),
    route!(GET "/api/w/{id}/settings/review", review::review_settings, tool!("get_review_settings", Reads,
        "The workspace's settings for the agent's review, the queue's first gate: whether it is on (by default it is, once the workspace has a model key), whether the gate waits for every review thread on a PR to be resolved (`require_resolved`, on by default), and how sure the agent must be to send each kind of change back to the PR's author to fix; `always_ask` asks a human about the kind instead. With the defaults, and each kind's findings of the last 90 days by confidence, with how often a human overruled the agent. Needs a Mergepatrol sign-in.")),
    route!(PUT "/api/w/{id}/settings/review", review::set_review_settings, tool!("set_review_settings", Writes,
        "Sets the workspace's settings for the agent's review. A repo's config file can set its own, in `review`. Needs a Mergepatrol sign-in.",
        arg("settings", Ty::Body, "`{ enabled, bars, require_resolved }`: whether the repos' queues review queued PRs first; each kind's bar, `[{ kind, bar }]`, with kind `security`, `migrations`, `dependencies`, `public_api` or `other`, and bar a percent from 50 to 100: how sure the agent must be to send a change back to the author, lower sending more; or `\"always_ask\"`, to ask a human about the kind. A kind left out keeps its default; and optionally whether the gate waits for every review thread to be resolved, left out keeping what is saved."))),
    route!(GET "/api/w/{id}/billing", billing::billing, tool!("get_billing", Reads,
        "The workspace's plan: trial or subscription, its seats, and the price. Null when self-hosted. Needs a Mergepatrol sign-in.")),
    route!(POST "/api/w/{id}/billing/checkout", billing::checkout, tool!("start_checkout", Writes,
        "A Stripe checkout page to subscribe the workspace, for the user to open. Needs a Mergepatrol sign-in.")),
    route!(POST "/api/w/{id}/billing/portal", billing::portal, tool!("open_billing_portal", Writes,
        "A Stripe page where the user manages the workspace's subscription. Needs a Mergepatrol sign-in.")),
    route!(DELETE "/api/w/{id}/billing/seats/{login}", billing::free_seat, tool!("free_seat", Destroys,
        "Frees a seat. The subscription drops it at its next period. Needs a Mergepatrol sign-in.")),
];

pub(crate) fn router() -> Router<AppState> {
    ROUTES.iter().fold(Router::new(), |r, route| {
        r.route(route.path, (route.handler)())
    })
}

pub(crate) fn tools() -> impl Iterator<Item = (&'static Route, &'static Tool)> {
    ROUTES.iter().filter_map(|route| match &route.agent {
        Agent::Tool(tool) => Some((route, tool)),
        Agent::Exempt(_) => None,
    })
}

impl Route {
    /// The path parameters, as `PATH_ARGS` has them, in order. `{name}` goes with `{owner}`.
    pub fn path_args(
        &self,
    ) -> impl Iterator<Item = &'static (&'static str, &'static str, Ty, &'static str)> {
        self.path
            .split('/')
            .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
            .filter(|&p| p != "name")
            .map(|p| {
                PATH_ARGS
                    .iter()
                    .find(|(param, ..)| *param == p)
                    .unwrap_or_else(|| panic!("no argument for {{{p}}}"))
            })
    }

    /// The repo that names the workspace: one in the path, or a `repo` argument.
    pub fn takes_repo(&self, tool: &Tool) -> bool {
        self.path.contains("{owner}") || tool.args.iter().any(|a| a.name == "repo")
    }
}

impl Tool {
    /// For `tools/list`.
    pub fn listing(&self, route: &Route) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        for (param, name, ty, description) in route.path_args() {
            properties.insert((*name).into(), ty.schema(description));
            // The repo names the workspace.
            if !(*param == "id" && route.takes_repo(self)) {
                required.push(*name);
            }
        }
        for arg in self.args {
            properties.insert(arg.name.into(), arg.ty.schema(arg.description));
            if arg.required {
                required.push(arg.name);
            }
        }
        let annotations = match self.effect {
            Effect::Reads => json!({ "readOnlyHint": true }),
            Effect::Writes => json!({ "readOnlyHint": false, "destructiveHint": false }),
            Effect::Destroys => json!({ "readOnlyHint": false, "destructiveHint": true }),
        };
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": { "type": "object", "properties": properties, "required": required },
            "annotations": annotations,
        })
    }
}

impl Ty {
    fn schema(&self, description: &str) -> Value {
        let mut schema = match self {
            Ty::Int => json!({ "type": "integer" }),
            Ty::Str => json!({ "type": "string" }),
            Ty::Bool => json!({ "type": "boolean" }),
            Ty::Strs => json!({ "type": "array", "items": { "type": "string" } }),
            Ty::OneOf(values) => json!({ "type": "string", "enum": values }),
            Ty::Body => json!({ "type": "object" }),
        };
        schema["description"] = description.into();
        schema
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// A route added outside the table would be one no agent can call, and no one chose that.
    #[test]
    fn the_server_routes_only_the_table() {
        let lib = include_str!("lib.rs");
        assert_eq!(lib.matches(".route(").count(), 0, "add routes to `ROUTES`");
    }

    #[test]
    fn each_tool_has_its_own_name_and_known_arguments() {
        let mut names = HashSet::new();
        for (route, tool) in tools() {
            assert!(names.insert(tool.name), "two tools named {}", tool.name);
            let mut args: Vec<&str> = route.path_args().map(|(_, name, ..)| *name).collect();
            args.extend(tool.args.iter().map(|a| a.name));
            let unique: HashSet<_> = args.iter().collect();
            assert_eq!(unique.len(), args.len(), "{}: an argument twice", tool.name);
            let bodies = tool
                .args
                .iter()
                .filter(|a| matches!(a.ty, Ty::Body))
                .count();
            assert!(
                bodies == 0 || (bodies == 1 && tool.args.len() == 1 && route.method != Method::GET),
                "{}: a body argument stands alone, on a route with a body",
                tool.name
            );
        }
    }

    #[test]
    fn every_api_route_is_a_tool_or_says_why_not() {
        for route in ROUTES {
            if let Agent::Exempt(why) = route.agent {
                assert!(!why.is_empty(), "{} {}: say why", route.method, route.path);
                assert!(
                    !route.path.starts_with("/api/w/"),
                    "{} {}: the dashboard's workspace calls are an agent's too",
                    route.method,
                    route.path
                );
            }
        }
    }

    /// The README's table of tools is the table here.
    #[test]
    fn the_readme_lists_every_tool() {
        let readme = include_str!("../../../README.md");
        let start = readme
            .find("<!-- tools:")
            .expect("the README marks its table of tools");
        let table = &readme[start..readme.find("<!-- /tools -->").unwrap()];
        let listed: Vec<&str> = table
            .lines()
            .filter_map(|l| l.strip_prefix("| `")?.split_once('`'))
            .map(|(name, _)| name)
            .collect();
        let tools: Vec<&str> = tools().map(|(_, t)| t.name).collect();
        assert_eq!(
            listed, tools,
            "README.md lists the tools in the order of `ROUTES`"
        );
    }
}
