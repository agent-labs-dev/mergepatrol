//! The app's state: who is signed in, and the latest snapshot of their queues, polled.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{App, AppContext, Context, Entity, Task};

use crate::api::{ApiError, Client};
use crate::feed::{self, Feed, Snapshot};
use crate::{keychain, sign_in};

const POLL: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_secs(30);
/// The step times move slowly, and cost the server four queries.
const STATS_EVERY: u32 = 30;
const STATS_DAYS: u32 = 7;
/// "Just landed" looks back this far.
const EXITS_DAYS: u32 = 1;

pub enum Auth {
    SignedOut { error: Option<String> },
    SigningIn,
    SignedIn { client: Client, login: String },
}

pub struct AppModel {
    pub server: String,
    pub auth: Auth,
    snapshot: Option<Snapshot>,
    pub fetched_at: Option<Instant>,
    /// The last poll's failure, while the snapshot before it still shows.
    pub error: Option<String>,
    polls: u32,
    poll: Option<Task<()>>,
}

impl AppModel {
    pub fn init(server: String, cx: &mut App) -> Entity<Self> {
        let token = keychain::read(&server);
        let model = cx.new(|_| AppModel {
            server,
            auth: Auth::SignedOut { error: None },
            snapshot: None,
            fetched_at: None,
            error: None,
            polls: 0,
            poll: None,
        });
        if let Some(token) = token {
            model.update(cx, |m, cx| m.start_session(token, cx));
        }
        model
    }

    pub fn feed(&self) -> Option<Feed> {
        let snapshot = self.snapshot.as_ref()?;
        Some(feed::build(snapshot, jiff::Timestamp::now()))
    }

    pub fn sign_in(&mut self, cx: &mut Context<Self>) {
        if matches!(self.auth, Auth::SigningIn) {
            return;
        }
        self.auth = Auth::SigningIn;
        cx.notify();
        let server = self.server.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { sign_in::run(&server) })
                .await;
            let _ = this.update(cx, |m, cx| match result {
                Ok(token) => {
                    if let Err(e) = keychain::write(&m.server, &token) {
                        log::warn!("could not keep the session in the Keychain: {e}");
                    }
                    m.start_session(token, cx);
                }
                Err(error) => {
                    m.auth = Auth::SignedOut { error: Some(error) };
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        if let Auth::SignedIn { client, .. } = &self.auth {
            let client = client.clone();
            cx.background_executor()
                .spawn(async move { client.sign_out() })
                .detach();
        }
        keychain::delete(&self.server);
        self.signed_out(None, cx);
    }

    fn signed_out(&mut self, error: Option<String>, cx: &mut Context<Self>) {
        self.auth = Auth::SignedOut { error };
        self.snapshot = None;
        self.fetched_at = None;
        self.error = None;
        self.poll = None;
        cx.notify();
    }

    /// Checks the token, then polls.
    fn start_session(&mut self, token: String, cx: &mut Context<Self>) {
        let client = Client::new(&self.server, &token);
        self.poll = None;
        self.auth = Auth::SigningIn;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let check = client.clone();
            let session = cx
                .background_executor()
                .spawn(async move { check.session() })
                .await;
            let _ = this.update(cx, |m, cx| match session {
                Ok(session) => match session.user {
                    Some(user) => {
                        m.auth = Auth::SignedIn {
                            client,
                            login: user.login,
                        };
                        m.start_polling(cx);
                    }
                    None => {
                        keychain::delete(&m.server);
                        m.signed_out(Some("Your session expired. Sign in again.".into()), cx);
                    }
                },
                Err(ApiError::SignedOut) => {
                    keychain::delete(&m.server);
                    m.signed_out(Some("Your session expired. Sign in again.".into()), cx);
                }
                // Offline at launch: keep the token, and try again.
                Err(ApiError::Other(e)) => {
                    m.signed_out(Some(format!("Could not reach the server: {e}")), cx);
                    m.poll = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(RETRY).await;
                        let _ = this.update(cx, |m, cx| m.start_session(token, cx));
                    }));
                }
            });
        })
        .detach();
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        self.polls = 0;
        self.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some((client, stats, p50))) = this.update(cx, |m, _| {
                    let Auth::SignedIn { client, .. } = &m.auth else {
                        return None;
                    };
                    let stats = m.polls.is_multiple_of(STATS_EVERY);
                    m.polls += 1;
                    let p50 = m.snapshot.as_ref().map(|s| s.p50.clone());
                    Some((client.clone(), stats, p50))
                }) else {
                    return;
                };
                let result = cx
                    .background_executor()
                    .spawn(async move { fetch(&client, stats, p50.unwrap_or_default()) })
                    .await;
                let keep_going = this
                    .update(cx, |m, cx| m.on_fetched(result, cx))
                    .unwrap_or(false);
                if !keep_going {
                    return;
                }
                cx.background_executor().timer(POLL).await;
            }
        }));
    }

    fn on_fetched(&mut self, result: Result<Snapshot, ApiError>, cx: &mut Context<Self>) -> bool {
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.fetched_at = Some(Instant::now());
                self.error = None;
            }
            Err(ApiError::SignedOut) => {
                keychain::delete(&self.server);
                self.signed_out(Some("Your session expired. Sign in again.".into()), cx);
                return false;
            }
            Err(ApiError::Other(e)) => {
                log::warn!("poll failed: {e}");
                self.error = Some(e);
            }
        }
        cx.notify();
        true
    }
}

/// One poll: every workspace's lanes, the events of each lane's running PR, the day's exits, and
/// now and then the step times.
fn fetch(
    client: &Client,
    with_stats: bool,
    mut p50: HashMap<crate::api::StepView, f64>,
) -> Result<Snapshot, ApiError> {
    // The server reads the user's workspaces from GitHub again each hour: ask each time.
    let session = client.session()?;
    if session.user.is_none() {
        return Err(ApiError::SignedOut);
    }
    let workspaces = session.workspaces;
    let mut snapshot = Snapshot::default();
    for ws in &workspaces {
        let id = ws.installation_id;
        let lanes = client.lanes(id)?;
        for lane in &lanes {
            let lead = lane
                .entries
                .iter()
                .find(|e| e.state == crate::api::EntryState::Active);
            if let Some(lead) = lead {
                let events = client.events(id, &lane.repo, lead.number)?;
                snapshot
                    .events
                    .insert((lane.repo.clone(), lead.number), events);
            }
        }
        snapshot.lanes.extend(lanes);
        snapshot.exits.extend(client.exits(id, EXITS_DAYS)?.exits);
    }
    if with_stats {
        // The first workspace with merges sets the typical times: one number per step is
        // enough for a progress bar.
        p50.clear();
        for ws in &workspaces {
            let steps = client.stats(ws.installation_id, STATS_DAYS)?.steps;
            if !steps.is_empty() {
                p50 = steps.into_iter().map(|s| (s.step, s.p50_secs)).collect();
                break;
            }
        }
    }
    snapshot.p50 = p50;
    // The timestamps are fixed-width UTC, so they sort as text.
    snapshot.exits.sort_by(|a, b| b.at.cmp(&a.at));
    Ok(snapshot)
}
