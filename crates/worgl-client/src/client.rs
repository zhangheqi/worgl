//! Typed calls to a Worgl server, one per route.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::RETRY_AFTER;
use reqwest::{Method, RequestBuilder, Response, header};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use worgl::issuer::LayerSummary;
use worgl::{
    BurnRequest, KeySet, MintRequest, Note, NoteStatus, Policy, PublicKey, RotationReport, Serial,
    Supply, TransferRequest,
};
use worgl_sdk::{MintInfo, MintView, Schedule};

use crate::error::{Error, Refusal};

/// How long a request may take, connection included.
const TIMEOUT: Duration = Duration::from_secs(30);

/// How long connecting may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Most bytes of an error answer without a code kept in [`Refusal::message`].
const MESSAGE_LIMIT: usize = 512;

/// A connection to one Worgl server, which may host many mints.
///
/// Every call is a single request with no retry. Transfers and burns are
/// idempotent, so resending one after an [`Error::is_transient`] failure is
/// safe; a [`Purse`](crate::Purse) does that itself. Cloning is cheap and
/// shares the connection pool.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
}

/// A mint's `info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    /// The mint's issuer key.
    pub issuer: PublicKey,
    /// Events the mint has applied since it was created.
    pub sequence: u64,
    /// Position new money is minted into.
    pub mint_layer: u32,
    /// Every layer, first to last.
    pub layers: Vec<LayerSummary>,
    /// Value minted, destroyed, burned and outstanding.
    pub supply: Supply,
    /// The rules in force.
    pub policy: Policy,
    /// When the mint rotates on its own; `None` when only on request.
    #[serde(default)]
    pub rotation: Option<Schedule>,
    /// Whether the mint is paused: it then refuses transfers, burns, mints
    /// and rotations.
    #[serde(default)]
    pub paused: bool,
}

impl Info {
    /// What a [`Holder`](worgl_sdk::Holder) needs of it.
    pub fn mint_info(&self) -> MintInfo {
        MintInfo {
            issuer: self.issuer,
            layers: self.layers.clone(),
            policy: self.policy.clone(),
            rotation: self.rotation,
            paused: self.paused,
        }
    }
}

/// What a playground faucet hands out, from `GET /v1/faucet`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaucetInfo {
    /// The mint its notes come from.
    pub mint: PublicKey,
    /// The value of each note.
    pub amount: u64,
    /// Notes one client address may get in any hour.
    pub per_address_per_hour: u32,
    /// Notes the faucet hands out in a day, UTC.
    pub per_day: u32,
    /// Notes left to hand out today.
    pub left_today: u32,
}

/// The mint's answer about a genuine note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verified {
    /// The note's serial.
    pub serial: Serial,
    /// Its amount.
    pub amount: u64,
    /// Whether it is live, spent, expired or unknown to the ledger.
    #[serde(flatten)]
    pub status: NoteStatus,
}

#[derive(Serialize)]
struct NoteRef<'a> {
    note: &'a Note,
}

#[derive(Deserialize)]
struct NoteBody {
    note: Note,
}

#[derive(Serialize)]
struct HolderRef<'a> {
    holder: &'a PublicKey,
}

#[derive(Deserialize)]
struct NotesBody {
    notes: Vec<Note>,
}

#[derive(Deserialize)]
struct BurnedBody {
    value: u128,
}

#[derive(Deserialize)]
struct PausedBody {
    #[expect(dead_code, reason = "the status code already says it worked")]
    paused: bool,
}

impl Client {
    /// A client for the server at `base_url`, such as
    /// `https://mint.example.com`. Requests time out after 30 seconds.
    pub fn new(base_url: &str) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("worgl-client/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()?;
        Self::with_http(http, base_url)
    }

    /// A client that sends its requests through `http`, for custom timeouts,
    /// proxies or certificates.
    pub fn with_http(http: reqwest::Client, base_url: &str) -> Result<Self, Error> {
        let url = reqwest::Url::parse(base_url).map_err(|_| Error::Url(base_url.into()))?;
        if !matches!(url.scheme(), "http" | "https") || url.cannot_be_a_base() {
            return Err(Error::Url(base_url.into()));
        }
        Ok(Self {
            http,
            base: url.as_str().trim_end_matches('/').to_owned(),
        })
    }

    /// The server URL requests go to.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// `GET /v1/mints/{mint}/info`.
    pub async fn info(&self, mint: &PublicKey) -> Result<Info, Error> {
        self.send(self.mint_route(Method::GET, mint, "info")).await
    }

    /// The mint's info as a [`MintView`], ready for a
    /// [`Holder`](worgl_sdk::Holder): its layers are the ones the server
    /// answered with, whatever this machine's clock says.
    pub async fn view(&self, mint: &PublicKey) -> Result<MintView, Error> {
        let info = self.info(mint).await?;
        Ok(MintView::new(&info.mint_info())?)
    }

    /// [`info`](Self::info), with the time the server answered: Unix seconds
    /// by its clock, from the answer's `Date` header, or by this machine's
    /// if the answer has no `Date` it can read.
    pub async fn info_and_time(&self, mint: &PublicKey) -> Result<(Info, u64), Error> {
        let answer = self
            .answer(self.mint_route(Method::GET, mint, "info"))
            .await?;
        let time = answered_at(&answer).unwrap_or_else(unix_now);
        Ok((answer.json().await?, time))
    }

    /// [`view`](Self::view), with the time the server answered, as
    /// [`info_and_time`](Self::info_and_time) reads it. Plan the transfers
    /// due before the mint's next rotation from it, as in
    /// [`Holder::refresh`](worgl_sdk::Holder::refresh): the mint rotates by
    /// its server's clock, and planning by this machine's, if it runs slow
    /// by more than the [margin](worgl_sdk::REFRESH_MARGIN_SECS), can put
    /// them past the rotation.
    pub async fn view_and_time(&self, mint: &PublicKey) -> Result<(MintView, u64), Error> {
        let (info, time) = self.info_and_time(mint).await?;
        Ok((MintView::new(&info.mint_info())?, time))
    }

    /// `GET /v1/mints/{mint}/keys`: the keys that sign the mint's live
    /// generations, each certified by the issuer key. Fails with
    /// [`Error::Keys`] if the set is another issuer's.
    pub async fn keys(&self, mint: &PublicKey) -> Result<KeySet, Error> {
        let keys: KeySet = self
            .send(self.mint_route(Method::GET, mint, "keys"))
            .await?;
        if keys.issuer() == mint {
            Ok(keys)
        } else {
            Err(Error::Keys)
        }
    }

    /// `POST /v1/verify`: what the note's mint says about it.
    ///
    /// Each call shows the mint the note, from this machine's address and at
    /// this time, so asking about several notes in a row invites the guess
    /// that they have one owner. To tell where the notes a holder holds
    /// stand, [`MintView::state`], with [`MintView::expires_at`] and
    /// [`MintView::spendable_at`], reads only the mint's info, from
    /// [`view`](Self::view), which says nothing about any one note.
    pub async fn verify(&self, note: &Note) -> Result<Verified, Error> {
        let request = self
            .route(Method::POST, "/v1/verify")
            .json(&NoteRef { note });
        self.send(request).await
    }

    /// `POST /v1/transfer`: the new notes, in the order of the outputs.
    pub async fn transfer(&self, request: &TransferRequest) -> Result<Vec<Note>, Error> {
        let request = self.route(Method::POST, "/v1/transfer").json(request);
        Ok(self.send::<NotesBody>(request).await?.notes)
    }

    /// `POST /v1/burn`: the value destroyed.
    pub async fn burn(&self, request: &BurnRequest) -> Result<u128, Error> {
        let request = self.route(Method::POST, "/v1/burn").json(request);
        Ok(self.send::<BurnedBody>(request).await?.value)
    }

    /// `POST /v1/mints/{mint}/mint`: a new note, for a request signed by one
    /// of the mint's authorities.
    pub async fn mint(&self, mint: &PublicKey, request: &MintRequest) -> Result<Note, Error> {
        let request = self.mint_route(Method::POST, mint, "mint").json(request);
        Ok(self.send::<NoteBody>(request).await?.note)
    }

    /// `GET /v1/faucet`, from a playground faucet: its mint and limits. The
    /// faucet is `worgl-faucet`, at this client's URL, which may be its
    /// mint's server with the faucet behind the same proxy.
    pub async fn faucet(&self) -> Result<FaucetInfo, Error> {
        self.send(self.route(Method::GET, "/v1/faucet")).await
    }

    /// `POST /v1/faucet`: a note of the faucet's mint, bound to `holder`.
    /// Past the faucet's limits it answers `429` with the code
    /// `rate_limited` or `exhausted`.
    pub async fn ask_faucet(&self, holder: &PublicKey) -> Result<Note, Error> {
        let request = self
            .route(Method::POST, "/v1/faucet")
            .json(&HolderRef { holder });
        Ok(self.send::<NoteBody>(request).await?.note)
    }

    /// `POST /v1/mints/{mint}/rotate`, with the mint's admin token.
    pub async fn rotate(&self, mint: &PublicKey, token: &str) -> Result<RotationReport, Error> {
        let request = self.mint_route(Method::POST, mint, "rotate");
        self.send(request.bearer_auth(token)).await
    }

    /// `POST /v1/mints/{mint}/pause`, with the mint's admin token.
    pub async fn pause(&self, mint: &PublicKey, token: &str) -> Result<(), Error> {
        let request = self.mint_route(Method::POST, mint, "pause");
        self.send::<PausedBody>(request.bearer_auth(token)).await?;
        Ok(())
    }

    /// `POST /v1/mints/{mint}/resume`, with the mint's admin token.
    pub async fn resume(&self, mint: &PublicKey, token: &str) -> Result<(), Error> {
        let request = self.mint_route(Method::POST, mint, "resume");
        self.send::<PausedBody>(request.bearer_auth(token)).await?;
        Ok(())
    }

    fn route(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, format!("{}{path}", self.base))
    }

    fn mint_route(&self, method: Method, mint: &PublicKey, route: &str) -> RequestBuilder {
        self.route(method, &format!("/v1/mints/{mint}/{route}"))
    }

    async fn send<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, Error> {
        Ok(self.answer(request).await?.json().await?)
    }

    /// Sends a request and returns the answer, or the refusal as an error.
    async fn answer(&self, request: RequestBuilder) -> Result<Response, Error> {
        let response = request.send().await?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(Error::Refused(refusal(response).await))
        }
    }
}

/// When an answer was made, in seconds since the Unix epoch by the server's
/// clock, from its `Date` header.
fn answered_at(response: &Response) -> Option<u64> {
    let date = response.headers().get(header::DATE)?.to_str().ok()?;
    let time = httpdate::parse_http_date(date).ok()?;
    Some(time.duration_since(UNIX_EPOCH).ok()?.as_secs())
}

/// Reads an error answer, keeping what it says even without a code.
async fn refusal(response: Response) -> Refusal {
    let status = response.status().as_u16();
    // In seconds; the server never sends the date form.
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
        .map(Duration::from_secs);
    let text = response.text().await.unwrap_or_default();
    match serde_json::from_str::<Refusal>(&text) {
        Ok(refusal) => Refusal {
            status,
            retry_after,
            ..refusal
        },
        Err(_) => {
            let mut message = text.trim().to_owned();
            if message.len() > MESSAGE_LIMIT {
                let mut end = MESSAGE_LIMIT;
                while !message.is_char_boundary(end) {
                    end -= 1;
                }
                message.truncate(end);
            }
            Refusal {
                status,
                code: "http".into(),
                message,
                retry_after,
            }
        }
    }
}

/// Seconds since the Unix epoch, as mints count time.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
