//! What the panel shows, built from what the server sent: what is merging now and how far along,
//! what needs a human, what is next, and what just left the queue.

use std::collections::HashMap;

use jiff::Timestamp;

use crate::api::{EntryState, EntryView, EventView, ExitView, LaneView, Outcome, StepView};

/// Everything one poll fetched, across the user's workspaces.
#[derive(Default, Clone)]
pub struct Snapshot {
    pub lanes: Vec<LaneView>,
    /// The first active PR's events in each lane, newest first, keyed by `(repo, pr)`.
    pub events: HashMap<(String, u32), Vec<EventView>>,
    /// Newest first.
    pub exits: Vec<ExitView>,
    /// The typical time in each step, from the history.
    pub p50: HashMap<StepView, f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Active, with no rebase yet in its latest run.
    Starting,
    Rebasing,
    Checks,
    Fixing,
    Held,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Starting => "starting",
            Stage::Rebasing => "rebasing",
            Stage::Checks => "checks",
            Stage::Fixing => "fixing",
            Stage::Held => "held",
        }
    }

    fn step(self) -> Option<StepView> {
        match self {
            Stage::Rebasing => Some(StepView::Rebasing),
            Stage::Checks => Some(StepView::Checks),
            Stage::Fixing => Some(StepView::Fixing),
            Stage::Starting | Stage::Held => None,
        }
    }
}

pub struct NowCard {
    pub repo: String,
    pub what: String,
    pub url: String,
    pub stage: Stage,
    /// Seconds in the stage, when the events tell.
    pub elapsed: Option<f64>,
    /// The stage's p50, from the history.
    pub typical: Option<f64>,
    pub note: String,
}

pub struct Row {
    pub repo: String,
    pub text: String,
    pub url: String,
}

pub struct Landed {
    pub merged: bool,
    pub text: String,
    pub url: String,
    /// Seconds from joining the queue to leaving it.
    pub took: Option<f64>,
    pub ago: f64,
}

pub struct Feed {
    pub now: Vec<NowCard>,
    pub held: Vec<Row>,
    pub next: Vec<Row>,
    pub landed: Vec<Landed>,
    pub active: usize,
    pub waiting: usize,
}

const NEXT_ROWS: usize = 5;
const LANDED_ROWS: usize = 6;

pub fn build(s: &Snapshot, now: Timestamp) -> Feed {
    let mut feed = Feed {
        now: Vec::new(),
        held: Vec::new(),
        next: Vec::new(),
        landed: Vec::new(),
        active: 0,
        waiting: 0,
    };
    // The waiting PRs, by place in their lane, so each lane's head comes before anyone's second.
    let mut waiting: Vec<(usize, Row)> = Vec::new();
    for lane in &s.lanes {
        let repo = short(&lane.repo);
        let active: Vec<&EntryView> = lane
            .entries
            .iter()
            .filter(|e| e.state == EntryState::Active)
            .collect();
        feed.active += active.len();
        if let Some(lead) = active.first() {
            let events = s
                .events
                .get(&(lane.repo.clone(), lead.number))
                .map(Vec::as_slice)
                .unwrap_or_default();
            let (stage, since) = stage_of(events);
            let what = match lane.batch {
                Some(batch) if active.len() > 1 => format!(
                    "Batch #{batch}: {}",
                    active
                        .iter()
                        .map(|e| format!("#{}", e.number))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                _ => format!("#{} {}", lead.number, lead.title),
            };
            feed.now.push(NowCard {
                repo: repo.clone(),
                what,
                url: lead.url.clone(),
                stage,
                elapsed: since.map(|at| secs_between(at, now)),
                typical: stage.step().and_then(|step| s.p50.get(&step).copied()),
                note: note(lead, active.len(), events, lane),
            });
        }
        let mut place = 0;
        for e in &lane.entries {
            match e.state {
                EntryState::Active => {}
                EntryState::Held => feed.held.push(Row {
                    repo: repo.clone(),
                    text: format!("#{} {}", e.number, e.detail.as_deref().unwrap_or("held")),
                    url: e.url.clone(),
                }),
                EntryState::Waiting => {
                    feed.waiting += 1;
                    waiting.push((
                        place,
                        Row {
                            repo: repo.clone(),
                            text: format!("#{} {}", e.number, e.title),
                            url: e.url.clone(),
                        },
                    ));
                    place += 1;
                }
            }
        }
    }
    waiting.sort_by_key(|(place, _)| *place);
    feed.next = waiting
        .into_iter()
        .take(NEXT_ROWS)
        .map(|(_, row)| row)
        .collect();
    feed.landed = s
        .exits
        .iter()
        .filter(|x| x.outcome != Outcome::Removed)
        .take(LANDED_ROWS)
        .filter_map(|x| {
            let at: Timestamp = x.at.parse().ok()?;
            let merged = x.outcome == Outcome::Merged;
            Some(Landed {
                merged,
                text: if merged {
                    format!("#{} {}", x.number, x.title)
                } else {
                    format!("#{} {}", x.number, x.detail)
                },
                url: x.url.clone(),
                took: x
                    .enqueued_at
                    .as_deref()
                    .and_then(|t| t.parse().ok())
                    .map(|from| secs_between(from, at)),
                ago: secs_between(at, now),
            })
        })
        .collect();
    feed
}

/// The stage a PR's latest run is in, and when it started, replaying the run's events the way
/// the dashboard's Activity does (`ui/src/Activity.tsx`). `events` are newest first.
pub fn stage_of(events: &[EventView]) -> (Stage, Option<Timestamp>) {
    // The latest run: back to the latest `enqueued`, then to the latest `rebasing` in it.
    let since_enqueued = match events.iter().position(|e| e.kind == "enqueued") {
        Some(i) => &events[..=i],
        None => events,
    };
    let Some(start) = since_enqueued.iter().position(|e| e.kind == "rebasing") else {
        return (Stage::Starting, None);
    };
    let mut stage = (Stage::Starting, None);
    for e in since_enqueued[..=start].iter().rev() {
        let at = e.at.parse().ok();
        match e.kind.as_str() {
            "rebasing" => stage = (Stage::Rebasing, at),
            "published" => stage = (Stage::Checks, at),
            "fixing" if is_attempt(&e.message) => stage = (Stage::Fixing, at),
            "held" => stage = (Stage::Held, at),
            _ => {}
        }
    }
    stage
}

/// The queue's own `fixing`, `… attempt 1 of 2`, not the job's echo of it.
fn is_attempt(message: &str) -> bool {
    let Some((_, tail)) = message.rsplit_once("attempt ") else {
        return false;
    };
    let mut parts = tail.split(" of ");
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), None) if a.parse::<u32>().is_ok() && b.parse::<u32>().is_ok()
    )
}

/// `prs` is how many PRs run together: a batch has no one author.
fn note(lead: &EntryView, prs: usize, events: &[EventView], lane: &LaneView) -> String {
    let mut parts = Vec::new();
    match &lead.author {
        _ if prs > 1 => parts.push(format!("{prs} PRs together")),
        Some(author) => parts.push(format!("by {}", author.login)),
        None => {}
    }
    if !lead.layers.is_empty() {
        parts.push(format!("stack of {}", lead.layers.len()));
    }
    let fix = events
        .iter()
        .find(|e| e.kind == "fixing" && is_attempt(&e.message))
        .and_then(|e| e.message.rsplit_once("attempt "))
        .map(|(_, n)| format!("fix {}", n.replace(" of ", "/")));
    if let Some(fix) = fix {
        parts.push(fix);
    }
    if lane.trunk != "main" {
        parts.push(format!("→ {}", lane.trunk));
    }
    parts.join(" · ")
}

fn secs_between(from: Timestamp, to: Timestamp) -> f64 {
    (to.as_millisecond() - from.as_millisecond()).max(0) as f64 / 1000.0
}

pub fn short(repo: &str) -> String {
    repo.split_once('/')
        .map_or(repo, |(_, name)| name)
        .to_owned()
}

/// `1h 4m`, `12m`, `40s`.
pub fn duration(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h {}m", s / 3600, s % 3600 / 60),
        _ => format!("{}d", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::PersonView;

    fn ev(at: &str, kind: &str, message: &str) -> EventView {
        EventView {
            at: format!("2026-10-29T10:{at}:00.000Z"),
            kind: kind.into(),
            message: message.into(),
        }
    }

    fn at(min: &str) -> Option<Timestamp> {
        format!("2026-10-29T10:{min}:00.000Z").parse().ok()
    }

    fn newest_first(mut events: Vec<EventView>) -> Vec<EventView> {
        events.reverse();
        events
    }

    #[test]
    fn a_run_moves_from_rebase_to_checks_to_fix() {
        let mut run = vec![ev("00", "enqueued", ""), ev("01", "rebasing", "onto main")];
        assert_eq!(
            stage_of(&newest_first(run.clone())),
            (Stage::Rebasing, at("01"))
        );
        run.push(ev("02", "published", &"a".repeat(40)));
        assert_eq!(
            stage_of(&newest_first(run.clone())),
            (Stage::Checks, at("02"))
        );
        // The job's echo of `fixing` is not a new stage; the queue's, with the attempt, is.
        run.push(ev("09", "fixing", "checks failed"));
        assert_eq!(
            stage_of(&newest_first(run.clone())),
            (Stage::Checks, at("02"))
        );
        run.push(ev("09", "fixing", "checks failed: attempt 1 of 2"));
        assert_eq!(
            stage_of(&newest_first(run.clone())),
            (Stage::Fixing, at("09"))
        );
        run.push(ev("12", "published", &"b".repeat(40)));
        assert_eq!(stage_of(&newest_first(run)), (Stage::Checks, at("12")));
    }

    #[test]
    fn a_new_rebase_starts_a_new_run() {
        let run = vec![
            ev("00", "enqueued", ""),
            ev("01", "rebasing", ""),
            ev("02", "published", ""),
            ev("05", "fixing", "attempt 1 of 2"),
            ev("08", "rebasing", "main moved"),
        ];
        assert_eq!(stage_of(&newest_first(run)), (Stage::Rebasing, at("08")));
    }

    #[test]
    fn requeued_starts_over() {
        let run = vec![
            ev("00", "enqueued", ""),
            ev("01", "rebasing", ""),
            ev("02", "published", ""),
            ev("03", "ejected", ""),
            ev("10", "enqueued", ""),
        ];
        assert_eq!(stage_of(&newest_first(run)), (Stage::Starting, None));
    }

    fn entry(number: u32, state: EntryState) -> EntryView {
        EntryView {
            number,
            state,
            detail: (state == EntryState::Held).then(|| "draft".into()),
            title: format!("PR {number}"),
            url: format!("https://github.com/o/r/pull/{number}"),
            author: Some(PersonView {
                login: "ada".into(),
            }),
            layers: Vec::new(),
        }
    }

    fn lane(repo: &str, batch: Option<u32>, entries: Vec<EntryView>) -> LaneView {
        LaneView {
            repo: repo.into(),
            trunk: "main".into(),
            entries,
            batch,
        }
    }

    #[test]
    fn builds_the_feed_across_lanes() {
        let s = Snapshot {
            lanes: vec![
                lane(
                    "o/a",
                    Some(9),
                    vec![
                        entry(1, EntryState::Active),
                        entry(2, EntryState::Active),
                        entry(3, EntryState::Waiting),
                        entry(4, EntryState::Waiting),
                        entry(5, EntryState::Held),
                    ],
                ),
                lane(
                    "o/b",
                    None,
                    vec![entry(6, EntryState::Active), entry(7, EntryState::Waiting)],
                ),
            ],
            events: HashMap::from([(
                ("o/b".to_owned(), 6),
                newest_first(vec![
                    ev("00", "enqueued", ""),
                    ev("01", "rebasing", ""),
                    ev("02", "published", ""),
                ]),
            )]),
            p50: HashMap::from([(StepView::Checks, 600.0)]),
            exits: vec![ExitView {
                number: 0,
                outcome: Outcome::Merged,
                detail: "abc".into(),
                title: "Old".into(),
                url: String::new(),
                enqueued_at: Some("2026-10-29T09:00:00.000Z".into()),
                at: "2026-10-29T09:30:00.000Z".into(),
            }],
        };
        let feed = build(&s, at("05").unwrap());
        assert_eq!(feed.active, 3);
        assert_eq!(feed.waiting, 3);
        assert_eq!(feed.now[0].what, "Batch #9: #1 #2");
        assert_eq!(feed.now[0].stage, Stage::Starting);
        assert_eq!(feed.now[0].note, "2 PRs together");
        assert_eq!(feed.now[1].what, "#6 PR 6");
        assert_eq!(feed.now[1].stage, Stage::Checks);
        assert_eq!(feed.now[1].elapsed, Some(180.0));
        assert_eq!(feed.now[1].typical, Some(600.0));
        // Each lane's head first.
        let next: Vec<&str> = feed.next.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(next, ["#3 PR 3", "#7 PR 7", "#4 PR 4"]);
        assert_eq!(feed.held[0].text, "#5 draft");
        assert_eq!(feed.landed[0].took, Some(1800.0));
        assert_eq!(feed.landed[0].ago, 35.0 * 60.0);
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(42.0), "42s");
        assert_eq!(duration(600.0), "10m");
        assert_eq!(duration(3900.0), "1h 5m");
        assert_eq!(duration(200_000.0), "2d");
    }
}
