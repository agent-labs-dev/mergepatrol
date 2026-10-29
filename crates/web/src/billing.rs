//! The workspace's plan in the dashboard: its subscription through Stripe, and its seats. Off
//! self-hosted, where `billing` reads `null`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use mergepatrol_billing::{Access, FAIR_USE_JOBS, FAIR_USE_PRS, PRICE_USD, TRIAL_DAYS};
use serde::Serialize;
use ts_rs::TS;

use crate::auth::SignedIn;
use crate::{ApiError, AppState};

const DAY: i64 = 24 * 60 * 60;

#[derive(Serialize, TS)]
#[ts(export)]
pub struct BillingView {
    pub access: AccessView,
    /// Days left in the trial, the last one partly.
    pub trial_days_left: Option<u32>,
    /// The developers with a seat.
    pub seats: Vec<String>,
    pub pricing: PricingView,
    /// It has a Stripe customer: the portal opens.
    pub manageable: bool,
}

/// The plan, for the landing page and the dashboard.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct PricingView {
    /// A private repo's price per seat, a month.
    pub price_usd: u32,
    pub trial_days: u32,
    /// Public repos' fair-use limit, an hour, across a workspace.
    pub fair_use_prs: u32,
    pub fair_use_jobs: u32,
}

impl PricingView {
    pub(crate) fn new() -> Self {
        Self {
            price_usd: PRICE_USD,
            trial_days: TRIAL_DAYS,
            fair_use_prs: FAIR_USE_PRS as u32,
            fair_use_jobs: FAIR_USE_JOBS as u32,
        }
    }
}

#[derive(Serialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AccessView {
    /// Never subscribed: the next subscription starts with a trial.
    New,
    Trial,
    Subscribed,
    /// The subscription ended.
    Ended,
}

/// Where to send the browser: a Stripe page.
#[derive(Serialize, TS)]
#[ts(export)]
pub struct RedirectView {
    pub url: String,
}

pub(crate) async fn billing(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
) -> Result<Json<Option<BillingView>>, ApiError> {
    let id = user.workspace(id)?;
    if app.billing.is_none() {
        return Ok(Json(None));
    }
    Ok(Json(Some(view(&app, id).await?)))
}

async fn view(app: &AppState, id: u64) -> Result<BillingView, ApiError> {
    let billing = app.billing.as_ref().ok_or_else(off)?;
    let status = billing.status(id).await?;
    let (access, trial_days_left) = match status.access {
        Access::None { tried: false } => (AccessView::New, None),
        Access::None { tried: true } => (AccessView::Ended, None),
        Access::Trial { ends_at } => {
            let left = (ends_at - now()).max(0) as u64;
            (AccessView::Trial, Some(left.div_ceil(DAY as u64) as u32))
        }
        Access::Subscribed => (AccessView::Subscribed, None),
    };
    Ok(BillingView {
        access,
        trial_days_left,
        seats: status.seats,
        pricing: PricingView::new(),
        manageable: status.customer,
    })
}

pub(crate) async fn checkout(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
) -> Result<Json<RedirectView>, ApiError> {
    let id = user.workspace(id)?;
    let billing = app.billing.as_ref().ok_or_else(off)?;
    let account = account(&app, id).await?;
    let url = billing.checkout(id, &account).await.map_err(stripe_error)?;
    tracing::info!(installation = id, "checkout started");
    Ok(Json(RedirectView { url }))
}

pub(crate) async fn portal(
    State(app): State<AppState>,
    user: SignedIn,
    Path(id): Path<u64>,
) -> Result<Json<RedirectView>, ApiError> {
    let id = user.workspace(id)?;
    let billing = app.billing.as_ref().ok_or_else(off)?;
    let account = account(&app, id).await?;
    let url = billing
        .portal(id, &account)
        .await
        .map_err(stripe_error)?
        .ok_or_else(|| ApiError::Status(StatusCode::CONFLICT, "Not subscribed yet.".into()))?;
    Ok(Json(RedirectView { url }))
}

/// Frees a seat. The subscription drops it at its next period.
pub(crate) async fn free_seat(
    State(app): State<AppState>,
    user: SignedIn,
    Path((id, login)): Path<(u64, String)>,
) -> Result<Json<BillingView>, ApiError> {
    let id = user.workspace(id)?;
    let billing = app.billing.as_ref().ok_or_else(off)?;
    if !billing.free_seat(id, &login).await? {
        return Err(ApiError::Status(
            StatusCode::NOT_FOUND,
            "no such seat".into(),
        ));
    }
    tracing::info!(installation = id, %login, "seat freed");
    Ok(Json(view(&app, id).await?))
}

/// Stripe's webhooks: a subscription started, changed or ended.
pub(crate) async fn webhook(
    State(app): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let billing = app.billing.as_ref().ok_or_else(off)?;
    let signature = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !billing.verify(signature, &body, now()) {
        return Err(ApiError::Status(
            StatusCode::UNAUTHORIZED,
            "bad signature".into(),
        ));
    }
    let event = serde_json::from_slice(&body).map_err(|_| ApiError::bad_request("not JSON"))?;
    // An error goes back, so Stripe sends the event again.
    billing.webhook(&event).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The workspace's GitHub account, which names it in the dashboard's URLs.
async fn account(app: &AppState, id: u64) -> Result<String, ApiError> {
    let workspace = app.store.workspaces(&[id]).await?.into_iter().next();
    Ok(workspace
        .ok_or_else(|| ApiError::Status(StatusCode::NOT_FOUND, "no such workspace".into()))?
        .account)
}

fn off() -> ApiError {
    ApiError::Status(StatusCode::NOT_FOUND, "billing is off".into())
}

fn stripe_error(e: anyhow::Error) -> ApiError {
    ApiError::Status(StatusCode::BAD_GATEWAY, format!("{e:#}"))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
