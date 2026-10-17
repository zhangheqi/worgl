//! A note's whole life, in memory: mint, split, pay, freeze, rotate, expire.
//!
//! Run with `cargo run --example lifecycle`.

use worgl::{
    Error, HolderKey, Issuer, IssuerKey, LedgerConfig, Note, NoteStatus, Output, Policy,
    TransferRequest,
};

/// A holder's notes, each with the one-time key it is bound to. This lives
/// with the holder; the issuer never sees it.
type Pocket = Vec<(Note, HolderKey)>;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ledger = LedgerConfig {
        layers: 4,
        layer_size: 1024,
    };
    let mut issuer = Issuer::new(IssuerKey::generate(), ledger, Policy::cash_like())?;
    println!("issuer {}\n", issuer.public_key());

    // Alice makes a one-time key and is minted 16 into layer 2 of 0..=3.
    let mut alice: Pocket = Vec::new();
    let key = HolderKey::generate();
    let note = issuer.mint(16, key.public_key())?;
    show(&issuer, "minted to Alice", &note);

    // Alice breaks the 16 into 8 + 4 + 2 + 1 + 1, each to a fresh key of her
    // own. The issuer sees one note become five, which it could already tell
    // were related. The split is a hop, so the new notes land in layer 3.
    let parts = [8, 4, 2, 1, 1].map(|amount| (amount, HolderKey::generate()));
    let outputs = parts
        .iter()
        .map(|(amount, k)| Output {
            amount: *amount,
            holder: k.public_key(),
        })
        .collect();
    let request = TransferRequest::sign(&[(note, &key)], outputs)?;
    for (note, (_, k)) in issuer.transfer(&request)?.into_iter().zip(parts) {
        alice.push((note, k));
    }
    show(&issuer, "Alice's 4 after the split", &alice[1].0);

    // Layer 3 is the last layer: those notes cannot move until a rotation.
    let refused = pay(&mut issuer, &mut alice, 4).unwrap_err();
    println!("Alice pays 4 right away: refused, {refused}\n");

    let report = issuer.rotate();
    println!(
        "rotation: generation {} dropped, {} notes destroyed\n",
        report.dropped, report.destroyed_notes
    );

    // Alice pays Bob 5 as two one-note transfers, 4 and 1, to two fresh keys
    // Bob made. Neither transfer reveals that the other exists.
    let mut bob: Pocket = Vec::new();
    for amount in [4, 1] {
        bob.push(pay(&mut issuer, &mut alice, amount)?);
    }
    show(&issuer, "Bob's 4", &bob[0].0);

    // Alice kept a copy of the 16 she split. Spending it again is refused.
    let again = TransferRequest::single(
        note,
        &key,
        Output {
            amount: 16,
            holder: HolderKey::generate().public_key(),
        },
    )?;
    let refused = issuer.transfer(&again).unwrap_err();
    println!("Alice replays her old 16: refused, {refused}\n");

    issuer.rotate();
    let mut carol: Pocket = Vec::new();
    carol.push(pay(&mut issuer, &mut bob, 4)?);
    show(&issuer, "Carol's 4", &carol[0].0);

    // Four rotations: Alice leaves her change alone, Carol refreshes hers by
    // paying it to a fresh key of her own once per period.
    for round in 1..=4 {
        let report = issuer.rotate();
        println!(
            "rotation {round}: {} notes worth {} destroyed",
            report.destroyed_notes, report.destroyed_value
        );
        let (note, key) = carol.remove(0);
        let fresh = HolderKey::generate();
        let output = Output {
            amount: note.amount(),
            holder: fresh.public_key(),
        };
        let request = TransferRequest::single(note, &key, output)?;
        carol.push((issuer.transfer(&request)?.remove(0), fresh));
    }
    println!();
    show(&issuer, "Alice's idle 8", &alice[0].0);
    show(&issuer, "Carol's refreshed 4", &carol[0].0);
    println!("supply {:?}", issuer.supply());
    Ok(())
}

/// Pays the first note of exactly `amount` from `from` to a fresh key the payee makes.
fn pay(issuer: &mut Issuer, from: &mut Pocket, amount: u64) -> Result<(Note, HolderKey), Error> {
    let i = from
        .iter()
        .position(|(note, _)| note.amount() == amount)
        .expect("the pocket holds a note of that amount");
    let (note, key) = &from[i];
    let payee = HolderKey::generate();
    let output = Output {
        amount,
        holder: payee.public_key(),
    };
    let request = TransferRequest::single(*note, key, output)?;
    let new = issuer.transfer(&request)?.remove(0);
    from.remove(i);
    Ok((new, payee))
}

fn show(issuer: &Issuer, label: &str, note: &Note) {
    match issuer.status(note).expect("genuine note") {
        NoteStatus::Live {
            position,
            hops_left,
            expires_after_rotations,
        } => println!(
            "{label}: serial {}, layer {position}, {hops_left} hops left, \
             dies after {expires_after_rotations} idle rotations\n",
            note.serial()
        ),
        other => println!("{label}: {other:?}\n"),
    }
}
