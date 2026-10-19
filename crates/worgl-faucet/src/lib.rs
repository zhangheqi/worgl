//! A faucet for a playground mint: anyone can ask for a note and get one,
//! within limits per client address and per day.
//!
//! The faucet is one of the mint's authorities, like any customer's minting
//! service. It holds an authority key the mint lists in its policy, signs a
//! [`MintRequest`] for each note it hands out and sends it to the mint's
//! server. Neither the server nor the issuer changes for it.
//!
//! | method | path         | body                 | answer |
//! |--------|--------------|----------------------|--------|
//! | POST   | `/v1/faucet` | `{"holder": <key>}`  | `{"note": <note>}` |
//! | GET    | `/v1/faucet` |                      | [`About`] |
//! | GET    | `/healthz`   |                      | `ok` |
//!
//! Errors answer like the server's, with a status code and `{"error": code,
//! "message": text}`:
//!
//! - `400 malformed`: the body is not `{"holder": <key>}`.
//! - `429 rate_limited`: this client address had its notes for the hour.
//!   `Retry-After` says when it may ask again.
//! - `429 exhausted`: the faucet handed out its notes for the day (UTC).
//!   `Retry-After` says when the next day starts.
//! - `503 paused` or `503 layer_full`: the mint refused, as the server
//!   documents.
//! - `503 unavailable`: the mint gave no clear answer in time. The note may
//!   still be minted, but only to the holder asked for.
//! - `500 internal`: the mint refused the faucet's request, as when the mint
//!   does not list the faucet's key. The faucet logs why.
//!
//! IPv6 clients are counted by their /64, since one host usually has a
//! whole /64 to pick addresses from. Every request counts against the
//! limits, whether or not the mint then answers.
//!
//! Mint requests go to the server one at a time, since the issuer accepts
//! an authority's requests only in rising order of their sequence numbers.
//! The faucet numbers them by microseconds since the Unix epoch, rising by
//! at least one each time, so it keeps no state across restarts; run one
//! faucet per authority key, on a clock that does not go back. A request
//! that gets no clear answer is sent again unchanged, to the next server
//! given if there are several, and the issuer answers a resent request with
//! the note it already made.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use worgl::{MintKey, MintRequest, Note, PublicKey};
use worgl_client::{Client, Error};

/// How long the faucet keeps sending a request the mint gives no clear
/// answer to.
const GIVE_UP: Duration = Duration::from_secs(30);

/// How long it waits before sending such a request again.
const RESEND_AFTER: Duration = Duration::from_millis(250);

const HOUR: Duration = Duration::from_secs(3600);

/// How much the faucet hands out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// The value of each note.
    pub amount: u64,
    /// Notes one client address may get in any hour.
    pub per_address_per_hour: u32,
    /// Notes the faucet hands out in a day, UTC.
    pub per_day: u32,
}

impl Default for Limits {
    /// Notes of 64, ten an hour per address and 10,000 a day.
    fn default() -> Self {
        Self {
            amount: 64,
            per_address_per_hour: 10,
            per_day: 10_000,
        }
    }
}

/// What the faucet needs to run.
pub struct Config {
    /// The mint's server, or several members of its cluster: a request with
    /// no clear answer goes again to the next one.
    pub servers: Vec<Client>,
    /// The mint's issuer key.
    pub mint: PublicKey,
    /// An authority key the mint lists in its policy.
    pub authority: MintKey,
    /// How much to hand out.
    pub limits: Limits,
    /// Count clients by the last address in `X-Forwarded-For`, as a proxy in
    /// front of the faucet appends it, rather than by the connection's.
    /// Turn it on only behind such a proxy: otherwise any client can name
    /// any address.
    pub behind_proxy: bool,
}

/// Answer of `GET /v1/faucet`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct About {
    /// The mint the notes come from.
    pub mint: PublicKey,
    /// The limits.
    #[serde(flatten)]
    pub limits: Limits,
    /// Notes left to hand out today.
    pub left_today: u32,
}

#[derive(Deserialize)]
struct Ask {
    holder: PublicKey,
}

#[derive(Serialize)]
struct Given {
    note: Note,
}

/// A running faucet. See the [crate documentation](crate).
pub struct Faucet {
    servers: Vec<Client>,
    mint: PublicKey,
    authority: MintKey,
    limits: Limits,
    behind_proxy: bool,
    /// Held while a request is with the mint.
    minting: tokio::sync::Mutex<Minting>,
    counts: Mutex<Counts>,
}

#[derive(Default)]
struct Minting {
    /// The last sequence number used.
    sequence: u64,
    /// The server that answered last.
    server: usize,
}

#[derive(Default)]
struct Counts {
    /// The UTC day `today` counts, as days since the Unix epoch.
    day: u64,
    today: u32,
    /// When each address asked within the last hour, oldest first.
    recent: HashMap<IpAddr, VecDeque<Instant>>,
    asks: u64,
}

/// Why the faucet handed out no note.
#[derive(Debug)]
struct Refused {
    status: StatusCode,
    code: &'static str,
    message: String,
    retry_after: Option<Duration>,
}

impl Refused {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: &'a str,
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: self.code,
            message: &self.message,
        };
        let mut response = (self.status, Json(body)).into_response();
        if let Some(after) = self.retry_after {
            let secs = after.as_secs() + u64::from(after.subsec_nanos() > 0);
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs.max(1)));
        }
        response
    }
}

impl Faucet {
    /// A faucet with `config`.
    ///
    /// # Panics
    ///
    /// If `config.servers` is empty.
    pub fn new(config: Config) -> Arc<Self> {
        assert!(!config.servers.is_empty(), "a faucet needs a server");
        Arc::new(Self {
            servers: config.servers,
            mint: config.mint,
            authority: config.authority,
            limits: config.limits,
            behind_proxy: config.behind_proxy,
            minting: tokio::sync::Mutex::default(),
            counts: Mutex::default(),
        })
    }

    /// What `GET /v1/faucet` answers.
    pub fn about(&self) -> About {
        let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        counts.roll(today());
        About {
            mint: self.mint,
            limits: self.limits,
            left_today: self.limits.per_day.saturating_sub(counts.today),
        }
    }

    /// The address a request counts against.
    fn client(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let forwarded = self
            .behind_proxy
            .then(|| headers.get_all("x-forwarded-for").iter().next_back())
            .flatten()
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit(',').next())
            .and_then(|last| last.trim().parse().ok());
        counted(forwarded.unwrap_or(peer))
    }

    /// Counts a request from `address`, or says why it may not have a note.
    fn take(&self, address: IpAddr) -> Result<(), Refused> {
        let now = Instant::now();
        let day = today();
        let mut counts = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        counts.roll(day);
        counts.asks += 1;
        if counts.asks.is_multiple_of(1024) {
            counts
                .recent
                .retain(|_, times| times.back().is_some_and(|&t| now - t < HOUR));
        }
        if counts.today >= self.limits.per_day {
            let mut refused = Refused::new(
                StatusCode::TOO_MANY_REQUESTS,
                "exhausted",
                "the faucet has handed out today's notes; ask again tomorrow (UTC)",
            );
            refused.retry_after = Some(until_tomorrow());
            return Err(refused);
        }
        let limit = self.limits.per_address_per_hour as usize;
        let times = counts.recent.entry(address).or_default();
        while times.front().is_some_and(|&t| now - t >= HOUR) {
            times.pop_front();
        }
        if times.len() >= limit {
            let mut refused = Refused::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                format!("{limit} notes an hour per address; ask again later"),
            );
            refused.retry_after = Some(times.front().map_or(HOUR, |&t| HOUR - (now - t)));
            return Err(refused);
        }
        times.push_back(now);
        counts.today += 1;
        Ok(())
    }

    /// Mints a note to `holder`. See the [crate documentation](crate).
    async fn mint_to(&self, holder: PublicKey) -> Result<Note, Refused> {
        let mut minting = self.minting.lock().await;
        let sequence = (minting.sequence + 1).max(micros_now());
        minting.sequence = sequence;
        let request = MintRequest::sign(
            &self.authority,
            &self.mint,
            sequence,
            self.limits.amount,
            holder,
        );
        let deadline = Instant::now() + GIVE_UP;
        loop {
            let server = &self.servers[minting.server];
            let error = match server.mint(&self.mint, &request).await {
                Ok(note) => return Ok(note),
                Err(error) => error,
            };
            let held = ["paused", "layer_full"]
                .into_iter()
                .find(|&code| error.code() == Some(code));
            match held {
                Some(code) => {
                    return Err(Refused::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        code,
                        format!("the mint refused: {error}"),
                    ));
                }
                None if !unclear(&error) => {
                    tracing::error!(%error, sequence, "the mint refused a faucet request");
                    return Err(Refused::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal",
                        "the mint refused the faucet's request",
                    ));
                }
                None if Instant::now() >= deadline => {
                    tracing::warn!(%error, sequence, "the mint gave no clear answer in time");
                    return Err(Refused::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        "the mint did not answer in time; ask again later",
                    ));
                }
                None => {
                    tracing::info!(%error, sequence, "sending a mint request again");
                    minting.server = (minting.server + 1) % self.servers.len();
                    tokio::time::sleep(RESEND_AFTER).await;
                }
            }
        }
    }
}

impl Counts {
    fn roll(&mut self, day: u64) {
        if day != self.day {
            self.day = day;
            self.today = 0;
        }
    }
}

/// Whether the mint may have made the note, or may make it if asked again:
/// no answer, or a `5xx` other than a refusal that holds until someone acts.
fn unclear(error: &Error) -> bool {
    match error {
        Error::Http(error) => !error.is_decode() && !error.is_builder(),
        Error::Refused(refusal) => refusal.status >= 500,
        _ => false,
    }
}

/// The address a client is counted by: its /64 for IPv6.
fn counted(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let [a, b, c, d, ..] = v6.segments();
                IpAddr::V6(Ipv6Addr::new(a, b, c, d, 0, 0, 0, 0))
            }
        },
    }
}

fn since_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

fn micros_now() -> u64 {
    u64::try_from(since_epoch().as_micros()).unwrap_or(u64::MAX)
}

fn today() -> u64 {
    since_epoch().as_secs() / 86_400
}

fn until_tomorrow() -> Duration {
    let now = since_epoch();
    Duration::from_secs((now.as_secs() / 86_400 + 1) * 86_400).saturating_sub(now)
}

/// The faucet's routes. Serve them with
/// [`into_make_service_with_connect_info::<SocketAddr>`](Router::into_make_service_with_connect_info),
/// since requests are counted by the client's address.
pub fn router(faucet: Arc<Faucet>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/faucet", get(about).post(give))
        .with_state(faucet)
}

async fn about(State(faucet): State<Arc<Faucet>>) -> Json<About> {
    Json(faucet.about())
}

async fn give(
    State(faucet): State<Arc<Faucet>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ask: Result<Json<Ask>, JsonRejection>,
) -> Result<Json<Given>, Refused> {
    let Json(ask) = ask.map_err(|rejection| {
        Refused::new(
            StatusCode::BAD_REQUEST,
            "malformed",
            format!("expected {{\"holder\": <key>}}: {}", rejection.body_text()),
        )
    })?;
    faucet.take(faucet.client(peer.ip(), &headers))?;
    let note = faucet.mint_to(ask.holder).await?;
    Ok(Json(Given { note }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_clients_count_by_their_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::2".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(counted(a), counted(b));
        assert_ne!(counted(a), counted(c));
        let mapped: IpAddr = "::ffff:192.0.2.7".parse().unwrap();
        assert_eq!(counted(mapped), "192.0.2.7".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn tomorrow_is_within_a_day() {
        let wait = until_tomorrow();
        assert!(wait > Duration::ZERO && wait <= Duration::from_secs(86_400));
    }
}
