//! `worgl`: hold, pay and receive Worgl notes from the command line.
//!
//! The purse lives in one file, encrypted under a passphrase. Payment
//! requests, payments and handed-over notes travel between people as JSON
//! files; `-` reads standard input or writes standard output.

mod vault;

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use worgl::{MintKey, MintRequest, Note, PublicKey};
use worgl_client::{Client, Purse, Submitted};
use worgl_sdk::{
    MintView, NoteState, Payment, PaymentRequest, REFRESH_MARGIN_SECS, ScheduledTransfer, Spacing,
};
use zeroize::Zeroizing;

use crate::vault::Vault;

/// Hold, pay and receive Worgl notes.
///
/// The purse file is encrypted under a passphrase, read from
/// WORGL_PASSPHRASE or asked for. Requests, payments and notes are JSON
/// files to send to the other side; `-` means standard input or output.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// The Worgl server.
    #[arg(
        long,
        global = true,
        env = "WORGL_SERVER",
        default_value = "http://127.0.0.1:8080"
    )]
    server: String,
    /// The purse file.
    #[arg(
        long,
        global = true,
        env = "WORGL_PURSE",
        default_value = "purse.worgl"
    )]
    purse: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an empty purse.
    Init,
    /// Show a mint's layers, rotation schedule and supply.
    Info {
        /// The mint's issuer key.
        mint: PublicKey,
    },
    /// Show what the purse holds at a mint.
    Balance {
        /// The mint's issuer key.
        mint: PublicKey,
    },
    /// List the notes held and what each can do now.
    Notes,
    /// Ask to be paid: write a payment request to send to the payer.
    Request {
        /// The mint whose notes to ask for.
        mint: PublicKey,
        /// The amount.
        amount: u64,
        /// Ask for notes no larger than this, for a payer whose notes are
        /// small.
        #[arg(long)]
        at_most: Option<u64>,
        /// Where to write the request.
        #[arg(long, short, default_value = "-")]
        out: PathBuf,
    },
    /// Stop expecting the notes of a request that will not be paid.
    Withdraw {
        /// The request.
        request: PathBuf,
    },
    /// Answer a payment request: write a payment to send to the payee, and
    /// say by when to settle it if the mint rotates on a schedule.
    Pay {
        /// The payee's request.
        request: PathBuf,
        /// Seconds between the transfers when the payment takes several
        /// notes, as MIN..MAX. Closer spacing lets the mint guess the notes
        /// had one owner.
        #[arg(long, default_value = "2..60", value_parser = parse_spacing)]
        spacing: Spacing,
        /// Where to write the payment.
        #[arg(long, short, default_value = "-")]
        out: PathBuf,
    },
    /// Take a payment: check it, submit each transfer at its time and keep
    /// the notes. Writes the payer's change from the mint's answers, for the
    /// payer to keep with `accept`.
    Receive {
        /// The request this pays.
        request: PathBuf,
        /// The payer's payment.
        payment: PathBuf,
        /// Where to write the payer's change.
        #[arg(long, short, default_value = "-")]
        out: PathBuf,
    },
    /// Submit a payment as the payer and write the notes for the payee, who
    /// keeps them with `accept`.
    Submit {
        /// The payment.
        payment: PathBuf,
        /// Where to write the notes.
        #[arg(long, short, default_value = "-")]
        out: PathBuf,
    },
    /// Keep notes the other side handed over, after checking their
    /// signatures: a payee's notes from `submit`, or a payer's change from
    /// `receive`, which settles the transfers it answers. Notes whose layer
    /// the mint has dropped are skipped.
    Accept {
        /// The notes, as `submit` or `receive` wrote them.
        notes: PathBuf,
    },
    /// Resend the transfers not yet seen answered and take in their change.
    Settle {
        /// Seconds between resends, as MIN..MAX.
        #[arg(long, default_value = "2..60", value_parser = parse_spacing)]
        spacing: Spacing,
    },
    /// Pay the notes that die at the next rotation back to the purse, each at
    /// a random moment before it, and resend the transfers not yet seen
    /// answered that must land before it. Waits until the last is done.
    Refresh {
        /// The mint's issuer key.
        mint: PublicKey,
        /// Submit them all at once. Faster, but the mint can then tell the
        /// notes belong together.
        #[arg(long)]
        now: bool,
    },
    /// Drop notes whose layer the mint has dropped.
    Prune {
        /// The mint's issuer key.
        mint: PublicKey,
    },
    /// Ask a playground faucet for a note and keep it. Prints the faucet's
    /// mint, for `balance` and `request`.
    Playground {
        /// The faucet, if not at the server's URL.
        #[arg(long, env = "WORGL_FAUCET")]
        faucet: Option<String>,
    },
    /// Mint notes into the purse, signing as one of the mint's authorities.
    Mint {
        /// The mint's issuer key.
        mint: PublicKey,
        /// The amount.
        amount: u64,
        /// File holding the authority's secret key, as `worgl-server
        /// create-mint` writes it.
        #[arg(long)]
        authority_key: PathBuf,
        /// Sequence number of the first note, above every one this key has
        /// used. Defaults to the time in microseconds.
        #[arg(long)]
        sequence: Option<u64>,
        /// Mint notes no larger than this.
        #[arg(long)]
        at_most: Option<u64>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let client = Client::new(&cli.server)?;
    let faucet = match &cli.command {
        Command::Playground { faucet: Some(url) } => Client::new(url)?,
        _ => client.clone(),
    };
    match cli.command {
        Command::Init => {
            let passphrase = new_passphrase(&cli.purse)?;
            Vault::create(&cli.purse, &passphrase)?;
            eprintln!("Created {}.", cli.purse.display());
            return Ok(());
        }
        Command::Info { mint } => return info(&client, &mint).await,
        _ => {}
    }
    let passphrase = passphrase(&cli.purse)?;
    let (vault, holder) = Vault::open(&cli.purse, &passphrase)?;
    let purse = Purse::new(client, holder, vault);
    match cli.command {
        Command::Init | Command::Info { .. } => unreachable!("handled above"),
        Command::Balance { mint } => balance(&purse, &mint).await,
        Command::Notes => notes(&purse).await,
        Command::Request {
            mint,
            amount,
            at_most,
            out,
        } => {
            let request = match at_most {
                Some(largest) => purse.request_at_most(&mint, amount, largest).await?,
                None => purse.request(&mint, amount).await?,
            };
            write(&out, &serde_json::to_string_pretty(&request)?)?;
            eprintln!(
                "Asked for {amount} in {} notes. Send the request to the payer.",
                request.outputs.len()
            );
            Ok(())
        }
        Command::Withdraw { request } => {
            let request: PaymentRequest = read_json(&request)?;
            purse.withdraw(&request)?;
            eprintln!("No longer expecting {}.", request.amount());
            Ok(())
        }
        Command::Pay {
            request,
            spacing,
            out,
        } => {
            let request: PaymentRequest = read_json(&request)?;
            // For the deadline; read first, so nothing fails once the payment is out.
            let view = purse.client().view(&request.mint).await?;
            let payment = purse.pay(&request, spacing).await?;
            write(&out, &serde_json::to_string_pretty(&payment)?)?;
            eprintln!(
                "Paid {} in {} transfer(s). Send the payment to the payee, then keep the \
                 change they send back with `worgl accept`, or run `worgl settle` once \
                 they have submitted it.",
                request.amount(),
                payment.transfers.len()
            );
            let now = unix_now();
            if let Some(by) = settle_by(&view, &payment, now) {
                let when = match by.saturating_sub(now) {
                    0 => "now".to_owned(),
                    left => format!("within {}", duration(left)),
                };
                eprintln!(
                    "Settle {when}, before the rotation that drops a note the payment spends."
                );
            }
            Ok(())
        }
        Command::Receive {
            request,
            payment,
            out,
        } => {
            let request: PaymentRequest = read_json(&request)?;
            let payment: Payment = read_json(&payment)?;
            announce(&payment.transfers);
            let submitted = purse.receive(&request, &payment).await?;
            write(&out, &serde_json::to_string_pretty(&submitted.theirs)?)?;
            if !submitted.theirs.is_empty() {
                eprintln!("Send the notes to the payer, who keeps the change with `worgl accept`.");
            }
            finish(&submitted, "Received")
        }
        Command::Submit { payment, out } => {
            let payment: Payment = read_json(&payment)?;
            announce(&payment.transfers);
            let submitted = purse.submit(&payment.transfers).await?;
            write(&out, &serde_json::to_string_pretty(&submitted.theirs)?)?;
            eprintln!("Send the notes to the payee, who keeps them with `worgl accept`.");
            finish(&submitted, "Kept in change")
        }
        Command::Accept { notes } => {
            let notes: Vec<Note> = read_json(&notes)?;
            let (notes, expired) = split_expired(purse.client(), notes).await?;
            // Only what the purse would have kept is lost: it ignores the
            // rest, as it does live notes for keys it never handed out.
            let mut probe = purse.holder();
            let held = probe.notes().count();
            let lost = probe.absorb(&expired);
            if lost > 0 {
                eprintln!(
                    "Skipped {} note(s) worth {lost}: the mint has dropped their layer.",
                    probe.notes().count() - held
                );
            }
            let value = purse.accept(&notes).await?;
            match purse.holder().outgoing().count() {
                0 => eprintln!("Kept {value}."),
                outgoing => eprintln!(
                    "Kept {value}. {outgoing} transfer(s) still outgoing; `worgl settle` \
                     resends them."
                ),
            }
            Ok(())
        }
        Command::Settle { spacing } => {
            let resent = purse.holder().resend(spacing);
            if resent.is_empty() {
                eprintln!("Nothing to settle.");
                return Ok(());
            }
            announce(&resent);
            let submitted = purse.submit(&resent).await?;
            finish(&submitted, "Took in")
        }
        Command::Refresh { mint, now } => {
            let mut plan = purse.plan_refresh(&mint).await?;
            if plan.is_empty() {
                eprintln!("No note dies at the next rotation.");
                return Ok(());
            }
            if now {
                for transfer in &mut plan {
                    transfer.after_secs = 0;
                }
            }
            announce(&plan);
            let submitted = purse.submit(&plan).await?;
            if !submitted.theirs.is_empty() {
                eprintln!(
                    "Resent a payment the payee may not have submitted yet: if so, it gets its \
                     notes with `worgl receive` while the mint keeps the answer."
                );
            }
            finish(&submitted, "Refreshed")
        }
        Command::Prune { mint } => {
            let lost = purse.prune(&mint).await?;
            eprintln!("Dropped {lost} in expired notes.");
            Ok(())
        }
        Command::Playground { .. } => {
            let (mint, value) = purse
                .take_from_faucet(&faucet)
                .await
                .context("asking the faucet")?;
            println!("{mint}");
            eprintln!("Kept {value} from the faucet. `worgl notes` shows when it expires.");
            Ok(())
        }
        Command::Mint {
            mint,
            amount,
            authority_key,
            sequence,
            at_most,
        } => {
            let key = read_mint_key(&authority_key)?;
            let first = sequence.unwrap_or_else(micros_now);
            let request = purse
                .request_at_most(&mint, amount, at_most.unwrap_or(amount))
                .await?;
            let mut minted = Vec::new();
            let mut failed = None;
            for (i, output) in (0u64..).zip(&request.outputs) {
                let signed = MintRequest::sign(
                    &key,
                    &mint,
                    first.saturating_add(i),
                    output.amount,
                    output.holder,
                );
                match purse.client().mint(&mint, &signed).await {
                    Ok(note) => minted.push(note),
                    Err(error) => {
                        failed = Some(error);
                        break;
                    }
                }
            }
            let value = purse.accept(&minted).await?;
            if let Some(error) = failed {
                purse.withdraw(&request)?;
                return Err(error).context(format!("minted only {value} of {amount}"));
            }
            eprintln!("Minted {value} in {} notes.", minted.len());
            Ok(())
        }
    }
}

async fn info(client: &Client, mint: &PublicKey) -> Result<()> {
    let info = client.info(mint).await?;
    println!("mint        {}", info.issuer);
    println!(
        "state       {}",
        if info.paused { "paused" } else { "running" }
    );
    match info.rotation {
        Some(schedule) => println!(
            "rotation    every {}, next in {}",
            duration(schedule.period_secs),
            duration(schedule.next_at.saturating_sub(unix_now()))
        ),
        None => println!("rotation    on request only"),
    }
    println!(
        "supply      {} outstanding ({} minted, {} expired, {} burned)",
        info.supply.outstanding, info.supply.minted, info.supply.destroyed, info.supply.burned
    );
    println!("layers      position  generation  live notes  value");
    for layer in &info.layers {
        let marker = if layer.position == info.mint_layer {
            "  (minted into)"
        } else {
            ""
        };
        println!(
            "            {:>8}  {:>10}  {:>10}  {}{marker}",
            layer.position, layer.generation, layer.live, layer.value
        );
    }
    Ok(())
}

async fn balance(purse: &Purse<Vault>, mint: &PublicKey) -> Result<()> {
    let view = purse.client().view(mint).await?;
    let balance = purse.holder().balance(&view);
    println!("spendable  {}", balance.spendable);
    println!(
        "frozen     {}  (moves after the next rotation)",
        balance.frozen
    );
    println!(
        "expiring   {}  (dies at the next rotation unless refreshed)",
        balance.expiring
    );
    println!(
        "outgoing   {}  (sent, not yet seen answered; see `worgl settle`)",
        balance.outgoing
    );
    println!(
        "expected   {}  (asked for, not yet received)",
        balance.expected
    );
    if balance.unknown > 0 {
        println!(
            "unknown    {}  (newer than the mint's info)",
            balance.unknown
        );
    }
    if let Some(left) = view.until_rotation(unix_now()) {
        println!("next rotation in {}", duration(left));
    }
    Ok(())
}

async fn notes(purse: &Purse<Vault>) -> Result<()> {
    let holder = purse.holder();
    let mut by_mint: BTreeMap<PublicKey, Vec<&Note>> = BTreeMap::new();
    for note in holder.notes() {
        by_mint.entry(*note.issuer()).or_default().push(note);
    }
    if by_mint.is_empty() {
        eprintln!("The purse holds no notes.");
    }
    for (mint, notes) in by_mint {
        let view: MintView = purse.client().view(&mint).await?;
        println!("mint {mint}");
        for note in notes {
            let state = match view.state(note) {
                NoteState::Spendable {
                    expires_after_rotations,
                } => format!("spendable, dies in {expires_after_rotations} rotation(s)"),
                NoteState::Frozen {
                    expires_after_rotations,
                } => format!(
                    "frozen until the next rotation, dies in {expires_after_rotations} rotation(s)"
                ),
                NoteState::Expired => "expired".into(),
                NoteState::Unknown => "newer than the mint's info".into(),
                NoteState::OtherMint => continue,
            };
            let serial = note.serial().to_string();
            println!("  {serial:>12}  {:>20}  {state}", note.amount());
        }
    }
    Ok(())
}

/// Says how long submitting will take.
fn announce(transfers: &[ScheduledTransfer]) {
    let last = transfers.iter().map(|t| t.after_secs).max().unwrap_or(0);
    if last == 0 {
        eprintln!("Submitting {} transfer(s).", transfers.len());
    } else {
        eprintln!(
            "Submitting {} transfer(s) over the next {}.",
            transfers.len(),
            duration(last)
        );
    }
}

/// When to have settled a payment by: the refresh margin before the
/// rotation that drops the first of the notes it spends, if the mint
/// rotates on a schedule.
fn settle_by(view: &MintView, payment: &Payment, now: u64) -> Option<u64> {
    payment
        .transfers
        .iter()
        .flat_map(|t| &t.request.inputs)
        .filter_map(|spend| view.expires_at(&spend.note, now))
        .min()
        .map(|at| at.saturating_sub(REFRESH_MARGIN_SECS))
}

/// Splits off the notes whose layer their mint has dropped: they no longer
/// verify, and are worth nothing.
async fn split_expired(client: &Client, notes: Vec<Note>) -> Result<(Vec<Note>, Vec<Note>)> {
    let mut oldest: BTreeMap<PublicKey, u64> = BTreeMap::new();
    let (mut live, mut expired) = (Vec::new(), Vec::new());
    for note in notes {
        let issuer = *note.issuer();
        if let Entry::Vacant(entry) = oldest.entry(issuer) {
            entry.insert(client.keys(&issuer).await?.oldest());
        }
        if note.serial().generation < oldest[&issuer] {
            expired.push(note);
        } else {
            live.push(note);
        }
    }
    Ok((live, expired))
}

/// Reports what submitting came to, failing if anything was refused.
fn finish(submitted: &Submitted, what: &str) -> Result<()> {
    eprintln!(
        "{what} {} in {} transfer(s).",
        submitted.value, submitted.answered
    );
    for refused in &submitted.refused {
        eprintln!("refused: {}", refused.error);
        match refused.at_stake {
            Some(0) => eprintln!("possibly applied: the mint no longer has its answer"),
            Some(value) => eprintln!(
                "possibly applied: the mint no longer has its answer, {value} may be lost"
            ),
            None => {}
        }
    }
    if submitted
        .refused
        .iter()
        .any(|r| r.at_stake.is_some_and(|value| value > 0))
    {
        eprintln!(
            "If the other side sent you notes from the mint's answers, keep them with \
             `worgl accept FILE`."
        );
    }
    let possibly_applied = submitted
        .refused
        .iter()
        .filter(|r| r.at_stake.is_some())
        .count();
    let others = submitted.refused.len() - possibly_applied;
    if others > 0 {
        bail!("{others} transfer(s) did not go through; try again later");
    }
    if possibly_applied > 0 {
        bail!("{possibly_applied} transfer(s) possibly applied, but not answered");
    }
    Ok(())
}

fn passphrase(purse: &Path) -> Result<Zeroizing<String>> {
    if let Some(passphrase) = env_passphrase() {
        return Ok(passphrase);
    }
    let prompt = format!("Passphrase for {}: ", purse.display());
    Ok(Zeroizing::new(
        rpassword::prompt_password(prompt).context("reading the passphrase")?,
    ))
}

fn new_passphrase(purse: &Path) -> Result<Zeroizing<String>> {
    let passphrase = match env_passphrase() {
        Some(passphrase) => passphrase,
        None => {
            let prompt = format!("New passphrase for {}: ", purse.display());
            let first = Zeroizing::new(rpassword::prompt_password(prompt)?);
            let again = Zeroizing::new(rpassword::prompt_password("Again: ")?);
            if first != again {
                bail!("the passphrases differ");
            }
            first
        }
    };
    if passphrase.is_empty() {
        bail!("the passphrase is empty");
    }
    Ok(passphrase)
}

fn env_passphrase() -> Option<Zeroizing<String>> {
    std::env::var("WORGL_PASSPHRASE").ok().map(Zeroizing::new)
}

fn parse_spacing(text: &str) -> Result<Spacing, String> {
    let (min, max) = text
        .split_once("..")
        .ok_or_else(|| format!("{text} is not MIN..MAX"))?;
    let min_secs = min
        .trim()
        .parse()
        .map_err(|_| format!("{min} is not a number"))?;
    let max_secs = max
        .trim()
        .parse()
        .map_err(|_| format!("{max} is not a number"))?;
    if min_secs > max_secs {
        return Err(format!("{min_secs} is above {max_secs}"));
    }
    Ok(Spacing { min_secs, max_secs })
}

fn read_mint_key(path: &Path) -> Result<MintKey> {
    let text = Zeroizing::new(
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
    );
    let mut secret = Zeroizing::new([0u8; 32]);
    hex::decode_to_slice(text.trim(), secret.as_mut())
        .with_context(|| format!("{} must hold 64 hex digits", path.display()))?;
    Ok(MintKey::from_secret_bytes(&secret))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text = if path == Path::new("-") {
        let mut text = String::new();
        io::stdin().read_to_string(&mut text)?;
        text
    } else {
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    };
    serde_json::from_str(&text).with_context(|| format!("reading {}", path.display()))
}

fn write(path: &Path, text: &str) -> Result<()> {
    if path == Path::new("-") {
        println!("{text}");
        Ok(())
    } else {
        fs::write(path, format!("{text}\n")).with_context(|| format!("writing {}", path.display()))
    }
}

fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs.div_ceil(60)),
        3600..86_400 => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
        _ => format!("{} d {} h", secs / 86_400, secs % 86_400 / 3600),
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn micros_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
        })
}
