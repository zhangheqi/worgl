//! `worgl`: hold, pay and receive Worgl notes from the command line.
//!
//! The purse lives in one file, encrypted under a passphrase. Payment
//! requests, payments and handed-over notes travel between people as JSON
//! files; `-` reads standard input or writes standard output.

mod vault;

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use worgl::{MintKey, MintRequest, Note, PublicKey, TransferRequest};
use worgl_client::{Client, Outcome, Purse, Submitted};
use worgl_sdk::{
    Holder, MadePayment, MintView, NoteState, PayError, Payment, PaymentRequest,
    REFRESH_MARGIN_SECS, ScheduledTransfer, Spacing,
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
    /// Show what the purse has at a mint: held, and on its way in or out.
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
    /// Keeps the payee's notes from the answers to a payment, in case the
    /// payee has not submitted it, and drops or cancels a transfer the mint
    /// refused for good. Fails while a refused transfer is kept outgoing.
    Settle {
        /// Seconds between resends, as MIN..MAX.
        #[arg(long, default_value = "2..60", value_parser = parse_spacing)]
        spacing: Spacing,
    },
    /// List the payments not yet done with, the transfers not yet seen
    /// answered and the notes kept for payees. Names each payment and
    /// transfer by an id, for `export`, `cancel` and `forget`.
    Outgoing,
    /// Write a payment again, as `pay` wrote it, for a payee who lost it,
    /// unless a transfer of it was cancelled or dropped. With `--notes`,
    /// write instead the payee's notes that `settle` or `refresh` kept from
    /// the mint's answers, for the payee to keep with `accept`, and stop
    /// keeping them.
    Export {
        /// The payment, by its id or one of its transfers' as `outgoing`
        /// lists them; the first digits will do.
        payment: String,
        /// Write the notes kept for the payee.
        #[arg(long)]
        notes: bool,
        /// Where to write it.
        #[arg(long, short, default_value = "-")]
        out: PathBuf,
    },
    /// Take back the note of an outgoing transfer the mint never applied,
    /// such as a payment the payee will never submit. Its change is still
    /// kept if handed back. Only for a transfer nobody will submit: if the
    /// mint applies it, or already had, the payee's notes and the note taken
    /// back claim the same value, and the mint refuses whichever is spent
    /// second.
    Cancel {
        /// The transfer, by its id as `outgoing` lists it; the first digits
        /// will do.
        transfer: String,
    },
    /// Drop an outgoing transfer whose note is gone. Its change is still
    /// kept if handed back. `settle` drops a transfer the mint refuses as
    /// spent or expired by itself; this is for one it keeps, such as one
    /// refused as unissued by a mint that lost its state.
    Forget {
        /// The transfer, by its id as `outgoing` lists it; the first digits
        /// will do.
        transfer: String,
    },
    /// Pay the notes that die at the next rotation back to the purse, each at
    /// a random moment before it, and resend the transfers not yet seen
    /// answered that must land before it, as `settle` does. Waits until the
    /// last is done.
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
            // For the deadline, by the mint's clock; read first, so nothing
            // fails once the payment is out.
            let (view, now) = purse.client().view_and_time(&request.mint).await?;
            let payment = match purse.pay(&request, spacing).await {
                Err(error @ worgl_client::Error::Pay(PayError::Paid(key))) => {
                    let holder = purse.holder();
                    let kept = holder
                        .payments()
                        .find(|made| made.request.outputs.iter().any(|o| o.holder == key));
                    return Err(match kept {
                        Some(made) => {
                            let id = payment_id(&made.request);
                            anyhow::Error::from(error).context(format!(
                                "payment {id} pays this request already; `worgl export {id} \
                                 -o FILE` writes it again"
                            ))
                        }
                        None => error.into(),
                    });
                }
                result => result?,
            };
            let id = payment_id(&request);
            serde_json::to_string_pretty(&payment)
                .map_err(anyhow::Error::from)
                .and_then(|json| write(&out, &json))
                .with_context(|| {
                    format!(
                        "the payment is recorded in the purse; write it again with \
                         `worgl export {id} -o FILE`"
                    )
                })?;
            eprintln!(
                "Paid {} in {} transfer(s), payment {id}. Send the payment to the payee, then \
                 keep the change they send back with `worgl accept`, or run `worgl settle` \
                 once they have submitted it.",
                request.amount(),
                payment.transfers.len()
            );
            if let Some(by) = settle_by(&view, &payment) {
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
            finish(&submitted, "Received", Fail::OnAny)
        }
        Command::Submit { payment, out } => {
            let payment: Payment = read_json(&payment)?;
            announce(&payment.transfers);
            let submitted = purse.submit(&payment.transfers).await?;
            let written = serde_json::to_string_pretty(&submitted.theirs)
                .map_err(anyhow::Error::from)
                .and_then(|json| write(&out, &json));
            if let Err(error) = written {
                // The submit kept them for a payment the purse made: they stay.
                let holder = purse.holder();
                let kept = holder
                    .payments()
                    .find(|made| made.owed.iter().any(|note| submitted.theirs.contains(note)));
                return Err(match kept {
                    Some(made) => error.context(format!(
                        "the payee's notes are kept in the purse; write them with `worgl export \
                         {} --notes -o FILE`",
                        payment_id(&made.request)
                    )),
                    None => error,
                });
            }
            // Handed over: the purse need not keep them for the payee.
            purse.forget_owed(&submitted.theirs)?;
            eprintln!("Send the notes to the payee, who keeps them with `worgl accept`.");
            finish(&submitted, "Kept in change", Fail::OnAny)
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
            kept_for_payees(&purse.holder(), &submitted);
            finish(&submitted, "Took in", Fail::OnKept)
        }
        Command::Outgoing => {
            outgoing(&purse.holder());
            Ok(())
        }
        Command::Export {
            payment,
            notes,
            out,
        } => {
            let holder = purse.holder();
            let made = find_payment(&holder, &payment)?;
            let id = payment_id(&made.request);
            if !notes {
                if made.request.check(&made.payment).is_err() {
                    bail!(
                        "payment {id} no longer pays its whole request, since a transfer of it \
                         was cancelled or dropped, so the payee would refuse it; `worgl \
                         outgoing` lists what it still has outgoing"
                    );
                }
                write(&out, &serde_json::to_string_pretty(&made.payment)?)?;
                eprintln!(
                    "Wrote payment {id} again. Send it to the payee, who takes it with \
                     `worgl receive`."
                );
                return Ok(());
            }
            if made.owed.is_empty() {
                bail!("no notes are kept for the payee of payment {id}");
            }
            write(&out, &serde_json::to_string_pretty(&made.owed)?)?;
            let value = purse.forget_owed(&made.owed)?;
            eprintln!(
                "Wrote {} note(s) worth {value} for the payee of payment {id}, who keeps them \
                 with `worgl accept`. The purse no longer keeps them.",
                made.owed.len()
            );
            Ok(())
        }
        Command::Cancel { transfer } => {
            let request = find_transfer(&purse.holder(), &transfer)?;
            if !purse.cancel(&request)? {
                bail!("transfer {} is no longer outgoing", transfer_id(&request));
            }
            eprintln!(
                "Cancelled transfer {}: its note of {} is back in the purse.",
                transfer_id(&request),
                spent(&request)
            );
            Ok(())
        }
        Command::Forget { transfer } => {
            let request = find_transfer(&purse.holder(), &transfer)?;
            if !purse.forget(&request)? {
                bail!("transfer {} is no longer outgoing", transfer_id(&request));
            }
            eprintln!(
                "Dropped transfer {} and its note of {}. Change handed back is still kept \
                 with `worgl accept`.",
                transfer_id(&request),
                spent(&request)
            );
            Ok(())
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
            kept_for_payees(&purse.holder(), &submitted);
            finish(&submitted, "Refreshed", Fail::OnKept)
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
    // For the time to the next rotation, by the mint's clock.
    let (info, now) = client.info_and_time(mint).await?;
    println!("mint        {}", info.issuer);
    println!(
        "state       {}",
        if info.paused { "paused" } else { "running" }
    );
    match info.rotation {
        Some(schedule) => println!(
            "rotation    every {}, next in {}",
            duration(schedule.period_secs),
            duration(schedule.next_at.saturating_sub(now))
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
    // For the time to the next rotation, by the mint's clock.
    let (view, now) = purse.client().view_and_time(mint).await?;
    // The total, then parts that do not overlap; expiring is part of what is
    // held, unchecked part of the total, and unconfirmed change is in no
    // total.
    let balance = purse.holder().balance(&view);
    println!(
        "total      {}  (held, plus change and refreshes on their way back)",
        balance.total()
    );
    println!("spendable  {}", balance.spendable);
    println!(
        "frozen     {}  (moves after the next rotation)",
        balance.frozen
    );
    if balance.unknown > 0 {
        println!(
            "unknown    {}  (newer than the mint's info)",
            balance.unknown
        );
    }
    println!(
        "expiring   {}  (dies at the next rotation unless refreshed)",
        balance.expiring
    );
    if balance.unchecked > 0 {
        println!(
            "unchecked  {}  (from another copy of the purse, which may have spent it since)",
            balance.unchecked
        );
    }
    println!(
        "change     {}  (from payments not yet seen answered)",
        balance.change
    );
    if balance.refreshing > 0 {
        println!(
            "refreshing {}  (refreshes not yet seen answered; see `worgl settle`)",
            balance.refreshing
        );
    }
    println!(
        "sending    {}  (paid, not yet seen answered; see `worgl settle`)",
        balance.sending
    );
    println!(
        "requested  {}  (asked for, not yet received)",
        balance.requested
    );
    if balance.unconfirmed > 0 {
        println!(
            "unconfirmed {}  (change of dropped or cancelled transfers, kept if handed back)",
            balance.unconfirmed
        );
    }
    if let Some(left) = view.until_rotation(now) {
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

/// Lists the payments kept, with their transfers still outgoing, and the
/// other outgoing transfers, by mint.
fn outgoing(holder: &Holder) {
    let pending: Vec<&TransferRequest> = holder.outgoing().collect();
    let mut by_mint: BTreeMap<PublicKey, (Vec<&MadePayment>, Vec<&TransferRequest>)> =
        BTreeMap::new();
    for made in holder.payments() {
        by_mint.entry(made.payment.mint).or_default().0.push(made);
    }
    for &request in &pending {
        let paid = holder
            .payments()
            .any(|made| made.payment.transfers.iter().any(|t| t.request == *request));
        if !paid && let Some(spend) = request.inputs.first() {
            by_mint
                .entry(*spend.note.issuer())
                .or_default()
                .1
                .push(request);
        }
    }
    if by_mint.is_empty() {
        eprintln!("Nothing is outgoing.");
    }
    for (mint, (payments, others)) in by_mint {
        println!("mint {mint}");
        for made in payments {
            let transfers = &made.payment.transfers;
            let left: Vec<&TransferRequest> = transfers
                .iter()
                .map(|t| &t.request)
                .filter(|request| pending.contains(request))
                .collect();
            // A transfer cancelled or dropped has left the payment.
            let asked = made.request.amount();
            let paying: u128 = made
                .request
                .outputs
                .iter()
                .filter(|wanted| transfers.iter().any(|t| t.request.outputs.contains(wanted)))
                .map(|wanted| u128::from(wanted.amount))
                .sum();
            let pays = if paying < asked {
                format!("{paying} of {asked}, the rest cancelled or dropped")
            } else {
                asked.to_string()
            };
            let mut line = format!(
                "  payment {}  pays {pays}, {} of {} transfer(s) outgoing",
                payment_id(&made.request),
                left.len(),
                transfers.len()
            );
            if !made.owed.is_empty() {
                let value: u128 = made.owed.iter().map(|n| u128::from(n.amount())).sum();
                line += &format!(
                    ", {} note(s) worth {value} kept for the payee",
                    made.owed.len()
                );
            }
            println!("{line}");
            for request in left {
                println!(
                    "    transfer {}  spends {}",
                    transfer_id(request),
                    spent(request)
                );
            }
        }
        for request in others {
            println!(
                "  transfer {}  spends {}",
                transfer_id(request),
                spent(request)
            );
        }
    }
}

/// Says where the payees' notes from the answers went: kept for payments
/// the purse made, or nowhere for one it has no record of.
fn kept_for_payees(holder: &Holder, submitted: &Submitted) {
    let kept = submitted
        .theirs
        .iter()
        .filter(|note| holder.payments().any(|made| made.owed.contains(note)))
        .count();
    if kept > 0 {
        eprintln!(
            "Kept {kept} note(s) for payees from the mint's answers, in case a payee has not \
             submitted its payment: `worgl outgoing` lists them, and `worgl export PAYMENT \
             --notes` writes them for the payee."
        );
    }
    if kept < submitted.theirs.len() {
        eprintln!(
            "Resent a payment the purse keeps no record of, such as one made by an earlier \
             release: if the payee has not submitted it yet, it gets its notes with `worgl \
             receive` while the mint keeps the answer."
        );
    }
}

/// A transfer's id as `outgoing` shows it: the first 12 hex digits of the
/// id its mint gives it.
fn transfer_id(request: &TransferRequest) -> String {
    let mut id = match request.inputs.first() {
        Some(spend) => request.id(spend.note.issuer()).to_string(),
        None => String::new(),
    };
    id.truncate(12);
    id
}

/// A payment's id as `outgoing` shows it: the first 12 hex digits of the
/// first key its request asks to be paid to. It stays the same whatever
/// becomes of the payment's transfers, and no two payments kept share that
/// key, since `pay` refuses a key it has paid already.
fn payment_id(request: &PaymentRequest) -> String {
    let mut id = request
        .outputs
        .first()
        .map_or_else(String::new, |o| o.holder.to_string());
    id.truncate(12);
    id
}

/// The value of the notes a transfer spends.
fn spent(request: &TransferRequest) -> u128 {
    request
        .inputs
        .iter()
        .map(|spend| u128::from(spend.note.amount()))
        .sum()
}

/// Whether `id` is the first digits of `full`.
fn starts(full: &str, id: &str) -> bool {
    let id = id.trim().to_ascii_lowercase();
    !id.is_empty() && full.starts_with(&id)
}

/// Whether `id` names a transfer: its full id starts with it.
fn names(request: &TransferRequest, id: &str) -> bool {
    request
        .inputs
        .first()
        .is_some_and(|spend| starts(&request.id(spend.note.issuer()).to_string(), id))
}

/// The outgoing transfer `id` names.
fn find_transfer(holder: &Holder, id: &str) -> Result<TransferRequest> {
    let found: Vec<&TransferRequest> = holder.outgoing().filter(|r| names(r, id)).collect();
    match found.as_slice() {
        [request] => Ok((*request).clone()),
        [] => bail!("no outgoing transfer has the id {id}; `worgl outgoing` lists them"),
        _ => bail!("several outgoing transfers have ids starting {id}; give more digits"),
    }
}

/// The payment kept that `id` names, by its id or one of its transfers'.
fn find_payment(holder: &Holder, id: &str) -> Result<MadePayment> {
    let found: Vec<&MadePayment> = holder
        .payments()
        .filter(|made| {
            let key = made.request.outputs.first().map(|o| o.holder.to_string());
            key.is_some_and(|key| starts(&key, id))
                || made.payment.transfers.iter().any(|t| names(&t.request, id))
        })
        .collect();
    match found.as_slice() {
        [made] => Ok((*made).clone()),
        [] => bail!("no payment kept has the id {id}; `worgl outgoing` lists them"),
        _ => bail!("several payments have ids starting {id}; give more digits"),
    }
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
fn settle_by(view: &MintView, payment: &Payment) -> Option<u64> {
    payment
        .transfers
        .iter()
        .flat_map(|t| &t.request.inputs)
        .filter_map(|spend| view.expires_at(&spend.note))
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

/// Which refused transfers fail a command.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fail {
    /// Any, for `receive` and `submit`: the payment did not go through.
    OnAny,
    /// Those kept outgoing for the next `settle`, for `settle` and
    /// `refresh`: one dropped or cancelled is done with.
    OnKept,
}

/// Reports what submitting came to, failing if a refused transfer is left
/// that `fail` counts.
fn finish(submitted: &Submitted, what: &str, fail: Fail) -> Result<()> {
    eprintln!(
        "{what} {} in {} transfer(s).",
        submitted.value, submitted.answered
    );
    for refused in &submitted.refused {
        eprintln!(
            "transfer {} refused: {}",
            transfer_id(&refused.request),
            refused.error
        );
        match refused.at_stake {
            Some(0) => eprintln!("possibly applied: the mint no longer has its answer"),
            Some(value) => eprintln!(
                "possibly applied: the mint no longer has its answer, {value} may be lost"
            ),
            None => {}
        }
        match refused.outcome {
            Outcome::Kept => {}
            Outcome::Dropped => eprintln!(
                "dropped: the note it spends is gone either way; its change is still kept if \
                 handed back"
            ),
            Outcome::Cancelled => eprintln!(
                "cancelled: the mint will not apply it, so the note it spends is back in the \
                 purse"
            ),
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
    let left: Vec<_> = submitted
        .refused
        .iter()
        .filter(|r| fail == Fail::OnAny || r.outcome == Outcome::Kept)
        .collect();
    if left.is_empty() {
        return Ok(());
    }
    if left.iter().all(|r| r.error.is_transient()) {
        bail!(
            "{} transfer(s) did not go through; try again later",
            left.len()
        );
    }
    if fail == Fail::OnKept {
        bail!(
            "{} transfer(s) refused and kept outgoing: `worgl settle` sends them again, and \
             `worgl cancel` or `worgl forget` ends one that cannot go through",
            left.len()
        );
    }
    if left.iter().all(|r| r.at_stake.is_some()) {
        bail!(
            "{} transfer(s) possibly applied, but not answered",
            left.len()
        );
    }
    bail!("{} transfer(s) did not go through", left.len())
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

/// A passphrase for a new purse file. The vault refuses an empty one.
fn new_passphrase(purse: &Path) -> Result<Zeroizing<String>> {
    if let Some(passphrase) = env_passphrase() {
        return Ok(passphrase);
    }
    let prompt = format!("New passphrase for {}: ", purse.display());
    let first = Zeroizing::new(rpassword::prompt_password(prompt)?);
    let again = Zeroizing::new(rpassword::prompt_password("Again: ")?);
    if first != again {
        bail!("the passphrases differ");
    }
    Ok(first)
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
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{text}")
            .and_then(|()| stdout.flush())
            .context("writing to standard output")
    } else {
        let text = format!("{text}\n");
        match fs::symlink_metadata(path) {
            // A link, a device or a pipe, such as /dev/stdout, a link to
            // /proc/self/fd/1: written in place, through the link.
            Ok(found) if !found.is_file() => fs::write(path, text),
            found => replace(path, &text, found.ok()),
        }
        .with_context(|| format!("writing {}", path.display()))
    }
}

/// Writes `text` to a file beside `path`, with the permissions of the file
/// `existing` describes if there is one, and renames it over `path` once it
/// is on disk, as the purse is saved: a failure leaves no half-written file.
fn replace(path: &Path, text: &str, existing: Option<fs::Metadata>) -> io::Result<()> {
    let name = path
        .file_name()
        .map_or_else(|| "out".into(), |name| name.to_string_lossy());
    let temporary = path.with_file_name(format!(".{name}.new"));
    let mut file = fs::File::create(&temporary)?;
    if let Some(existing) = existing {
        file.set_permissions(existing.permissions())?;
    }
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    // The rename is durable only once the directory is flushed too.
    #[cfg(unix)]
    {
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs.div_ceil(60)),
        3600..86_400 => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
        _ => format!("{} d {} h", secs / 86_400, secs % 86_400 / 3600),
    }
}

fn micros_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
        })
}
