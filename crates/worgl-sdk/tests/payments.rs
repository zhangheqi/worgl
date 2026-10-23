//! Payments between holders, carried out by a real issuer in memory.

use std::collections::HashSet;

use proptest::prelude::*;
use worgl::{HolderKey, Issuer, IssuerKey, LedgerConfig, Note, Output, Policy};
use worgl_sdk::{
    Balance, CheckError, Holder, MintInfo, MintView, NoteState, PayError, Payment, PaymentRequest,
    REFRESH_MARGIN_SECS, Schedule, Spacing,
};

fn issuer(layers: u32) -> Issuer {
    issuer_with(layers, Policy::cash_like())
}

fn issuer_with(layers: u32, policy: Policy) -> Issuer {
    let ledger = LedgerConfig {
        layers,
        layer_size: 4096,
    };
    Issuer::new(IssuerKey::generate(), ledger, policy).unwrap()
}

fn view(issuer: &Issuer) -> MintView {
    MintView::new(&MintInfo {
        issuer: *issuer.public_key(),
        layers: issuer.layers(),
        policy: issuer.policy().clone(),
        rotation: None,
        paused: false,
    })
    .unwrap()
}

/// Mints `amounts` to fresh keys of `holder`, one note each.
fn fund(issuer: &mut Issuer, holder: &mut Holder, amounts: &[u64]) {
    for &amount in amounts {
        let request = holder
            .request_at_most(&view(issuer), amount, amount)
            .unwrap();
        let [output] = request.outputs.as_slice() else {
            panic!("{amount} is not one note");
        };
        let note = issuer.mint(output.amount, output.holder).unwrap();
        assert_eq!(holder.absorb(&[note]), u128::from(amount));
    }
}

/// Submits every transfer of a payment and returns the mint's answers.
fn submit(issuer: &mut Issuer, payment: &Payment) -> Vec<Vec<Note>> {
    payment
        .transfers
        .iter()
        .map(|t| issuer.transfer(&t.request).unwrap())
        .collect()
}

fn settle_all(holder: &mut Holder, payment: &Payment, answers: &[Vec<Note>]) -> u128 {
    payment
        .transfers
        .iter()
        .zip(answers)
        .map(|(t, notes)| holder.settle(&t.request, notes))
        .sum()
}

/// A holder's balance at `issuer`, once its sums are checked against its
/// parts.
fn balance(holder: &Holder, issuer: &Issuer) -> Balance {
    let b = holder.balance(&view(issuer));
    assert_eq!(b.expected, b.requested + b.change + b.refreshing, "{b:?}");
    assert!(b.expiring <= b.spendable + b.frozen, "{b:?}");
    assert!(b.outgoing >= b.sending + b.change + b.refreshing, "{b:?}");
    b
}

/// The parts of a balance, which never overlap: held, sending, change,
/// refreshing and requested; then unconfirmed, which is apart from them.
fn parts(b: &Balance) -> [u128; 6] {
    [
        b.held(),
        b.sending,
        b.change,
        b.refreshing,
        b.requested,
        b.unconfirmed,
    ]
}

#[test]
fn one_note_pays_and_the_change_comes_back() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);

    let request = bob.request(&view(&mint), 5).unwrap();
    assert_eq!(request.amount(), 5);
    assert_eq!(request.outputs.len(), 2);
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    assert_eq!(payment.transfers.len(), 1);
    assert_eq!(payment.transfers[0].after_secs, 0);
    assert_eq!(payment.transfers[0].request.inputs.len(), 1);
    assert_eq!(request.check(&payment), Ok(()));
    assert_eq!(alice.balance(&view(&mint)).outgoing, 8);

    let answers = submit(&mut mint, &payment);
    assert_eq!(bob.absorb(&answers[0]), 5);
    assert_eq!(settle_all(&mut alice, &payment, &answers), 3);

    let v = view(&mint);
    // Received notes land in the last layer, frozen until the next rotation.
    assert_eq!(bob.balance(&v).frozen, 5);
    assert_eq!(bob.balance(&v).expected, 0);
    assert_eq!(alice.balance(&v).frozen, 3);
    assert_eq!(alice.balance(&v).outgoing, 0);
    assert_eq!(alice.outgoing().count(), 0);
}

#[test]
fn several_notes_pay_in_separate_spaced_transfers() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[4, 4]);

    let request = bob.request(&view(&mint), 6).unwrap();
    let spacing = Spacing {
        min_secs: 5,
        max_secs: 9,
    };
    let payment = alice.pay(&view(&mint), &request, spacing).unwrap();
    assert_eq!(payment.transfers.len(), 2);
    assert!(
        payment
            .transfers
            .iter()
            .all(|t| t.request.inputs.len() == 1)
    );
    let gap = payment.transfers[1].after_secs - payment.transfers[0].after_secs;
    assert!((5..=9).contains(&gap), "gap {gap}");
    assert_eq!(request.check(&payment), Ok(()));

    let answers = submit(&mut mint, &payment);
    let received: u128 = answers.iter().map(|notes| bob.absorb(notes)).sum();
    assert_eq!(received, 6);
    assert_eq!(settle_all(&mut alice, &payment, &answers), 2);
}

#[test]
fn small_notes_pay_a_request_for_small_notes() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[1; 8]);

    let request = bob.request(&view(&mint), 8).unwrap();
    assert_eq!(
        alice.pay(&view(&mint), &request, Spacing::default()),
        Err(PayError::Fragmented { largest: 1 })
    );
    bob.withdraw(&request);
    assert_eq!(alice.balance(&view(&mint)).spendable, 8);

    let request = bob.request_at_most(&view(&mint), 8, 1).unwrap();
    assert_eq!(request.outputs.len(), 8);
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    assert_eq!(payment.transfers.len(), 8);
    assert!(
        payment
            .transfers
            .windows(2)
            .all(|w| w[0].after_secs < w[1].after_secs)
    );
    let answers = submit(&mut mint, &payment);
    let received: u128 = answers.iter().map(|notes| bob.absorb(notes)).sum();
    assert_eq!(received, 8);
    assert_eq!(bob.balance(&view(&mint)).expected, 0);
}

#[test]
fn a_failed_payment_changes_nothing() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[4]);
    let before = alice.to_json();
    let request = bob.request(&view(&mint), 5).unwrap();
    assert_eq!(
        alice.pay(&view(&mint), &request, Spacing::default()),
        Err(PayError::Unaffordable {
            spendable: 4,
            needed: 5
        })
    );
    assert_eq!(alice.to_json(), before);

    let other = issuer(4);
    let request = bob.request(&view(&other), 1).unwrap();
    assert_eq!(
        alice.pay(&view(&mint), &request, Spacing::default()),
        Err(PayError::WrongMint)
    );
}

#[test]
fn frozen_notes_wait_for_the_next_rotation() {
    let mut mint = issuer(4);
    let (mut alice, mut bob, mut carol) = (Holder::new(), Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let request = bob.request(&view(&mint), 8).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    for notes in submit(&mut mint, &payment) {
        bob.absorb(&notes);
    }

    let request = carol.request(&view(&mint), 1).unwrap();
    assert!(matches!(
        bob.pay(&view(&mint), &request, Spacing::default()),
        Err(PayError::Unaffordable { spendable: 0, .. })
    ));
    let note = *bob.notes().next().unwrap();
    assert_eq!(
        view(&mint).state(&note),
        NoteState::Frozen {
            expires_after_rotations: 4
        }
    );

    mint.rotate().unwrap();
    let payment = bob.pay(&view(&mint), &request, Spacing::default()).unwrap();
    let answers = submit(&mut mint, &payment);
    assert_eq!(carol.absorb(&answers[0]), 1);
    assert_eq!(settle_all(&mut bob, &payment, &answers), 7);
}

#[test]
fn refreshing_keeps_notes_alive() {
    let mut mint = issuer(3);
    let mut alice = Holder::new();
    // Minted into the second to last layer, position 1 of 3.
    fund(&mut mint, &mut alice, &[2, 4]);
    mint.rotate().unwrap();
    let v = view(&mint);
    assert_eq!(alice.balance(&v).expiring, 6);

    let refreshes = alice.refresh(&v, 0);
    assert_eq!(refreshes.len(), 2);
    assert!(refreshes.iter().all(|t| t.after_secs <= 60));
    assert!(refreshes.iter().all(|t| t.request.inputs.len() == 1));
    // Refresh one note and let the other lapse.
    let answer = mint.transfer(&refreshes[0].request).unwrap();
    let refreshed = alice.settle(&refreshes[0].request, &answer);
    alice.cancel(&refreshes[1].request);
    assert_eq!(alice.balance(&view(&mint)).expiring, 6 - refreshed);

    mint.rotate().unwrap();
    let v = view(&mint);
    assert_eq!(alice.prune(&v), 6 - refreshed);
    assert_eq!(alice.balance(&v).held(), refreshed);
    let note = *alice.notes().next().unwrap();
    assert!(matches!(
        mint.status(&note),
        Ok(worgl::NoteStatus::Live { .. })
    ));
}

#[test]
fn checking_a_payment_catches_what_it_does_not_pay() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let request = bob.request(&view(&mint), 3).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();

    let mut short = payment.clone();
    let outputs = &mut short.transfers[0].request.outputs;
    let i = outputs
        .iter()
        .position(|o| o == &request.outputs[0])
        .unwrap();
    outputs[i].amount += 1;
    let tampered = outputs[i];
    assert_eq!(request.check(&short), Err(CheckError::Duplicate(tampered)));

    let mut missing = payment.clone();
    missing.transfers[0]
        .request
        .outputs
        .retain(|o| o != &request.outputs[0]);
    assert_eq!(
        request.check(&missing),
        Err(CheckError::Missing(request.outputs[0]))
    );

    let mut elsewhere = payment;
    elsewhere.mint = *issuer(4).public_key();
    assert_eq!(request.check(&elsewhere), Err(CheckError::WrongMint));
}

#[test]
fn a_lost_answer_is_recovered_by_resending() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let request = bob.request(&view(&mint), 3).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    // Bob submits; Alice never sees the answer.
    bob.absorb(&submit(&mut mint, &payment)[0]);

    // Alice reloads her store and resends what is outgoing.
    let mut alice = Holder::from_json(&alice.to_json()).unwrap();
    let pending: Vec<_> = alice.outgoing().cloned().collect();
    assert_eq!(pending.len(), 1);
    let answer = mint.transfer(&pending[0]).unwrap();
    assert_eq!(alice.settle(&pending[0], &answer), 5);
    assert_eq!(alice.outgoing().count(), 0);
}

#[test]
fn change_handed_back_settles_a_transfer_the_mint_no_longer_answers() {
    // Two receipts stand in for a mint whose receipts many transfers pushed out.
    let mut policy = Policy::cash_like();
    policy.receipt_capacity = 2;
    let mut mint = issuer_with(4, policy);
    let (mut alice, mut bob, mut carol) = (Holder::new(), Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    fund(&mut mint, &mut carol, &[1, 1, 1]);
    let request = bob.request(&view(&mint), 3).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let transfer = &payment.transfers[0].request;
    let answer = mint.transfer(transfer).unwrap();
    assert_eq!(bob.at_stake(transfer), Some(3));
    assert_eq!(bob.absorb(&answer), 3);
    assert_eq!(bob.at_stake(transfer), None);
    // What Bob hands back is Alice's change, none of his own notes.
    let change = bob.theirs(&answer);
    assert_eq!(change.len(), answer.len() - request.outputs.len());
    assert!(
        change
            .iter()
            .all(|n| request.outputs.iter().all(|o| o.holder != *n.holder()))
    );

    // Other holders' transfers push Alice's receipt out.
    for _ in 0..3 {
        let paid = Holder::new().request(&view(&mint), 1).unwrap();
        let payment = carol.pay(&view(&mint), &paid, Spacing::default()).unwrap();
        mint.transfer(&payment.transfers[0].request).unwrap();
    }
    let resend = alice.resend(Spacing::default());
    let error = mint.transfer(&resend[0].request).unwrap_err();
    assert!(matches!(error, worgl::Error::Spent(_)), "{error}");
    assert_eq!(alice.at_stake(&resend[0].request), Some(5));

    // The change Bob hands back recovers it and settles the transfer.
    assert!(change.iter().all(|n| n.verify(mint.keys()).is_ok()));
    assert_eq!(alice.take_answers(&change), 5);
    assert_eq!(alice.outgoing().count(), 0);
    assert_eq!(alice.at_stake(transfer), None);
    let balance = alice.balance(&view(&mint));
    assert_eq!(
        (balance.frozen, balance.outgoing, balance.expected),
        (5, 0, 0)
    );
}

#[test]
fn taking_answers_settles_a_transfer_only_once_all_its_change_is_back() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8, 8]);
    // 8 for 1: the change is 4, 2 and 1.
    let request = bob.request(&view(&mint), 1).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let transfer = &payment.transfers[0].request;
    let answer = mint.transfer(transfer).unwrap();
    let change = bob.theirs(&answer);
    assert_eq!(change.len(), 3);

    // Bob's own note is not Alice's to take, and settles nothing.
    let bobs: Vec<Note> = answer
        .iter()
        .filter(|n| !change.contains(n))
        .copied()
        .collect();
    assert_eq!(alice.take_answers(&bobs), 0);
    assert_eq!(alice.outgoing().count(), 1);

    let first = u128::from(change[0].amount());
    assert_eq!(alice.take_answers(&change[..1]), first);
    assert_eq!(alice.outgoing().count(), 1);
    assert_eq!(alice.at_stake(transfer), Some(7 - first));
    // The change taken in counts as held, and the rest as change, once each.
    let b = balance(&alice, &mint);
    assert_eq!(parts(&b), [8 + first, 1, 7 - first, 0, 0, 0]);
    assert_eq!((b.total(), b.outgoing), (15, 8));
    assert_eq!(alice.take_answers(&change[1..]), 7 - first);
    assert_eq!(alice.outgoing().count(), 0);

    // Settling the same answer afterwards, or taking it again, changes nothing.
    let after = alice.to_json();
    assert_eq!(alice.settle(transfer, &answer), 0);
    assert_eq!(alice.take_answers(&answer), 0);
    assert_eq!(alice.to_json(), after);

    // Nor does taking the answer after settling it.
    let request = bob.request(&view(&mint), 1).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let transfer = &payment.transfers[0].request;
    let answer = mint.transfer(transfer).unwrap();
    assert_eq!(alice.settle(transfer, &answer), 7);
    let after = alice.to_json();
    assert_eq!(alice.take_answers(&bob.theirs(&answer)), 0);
    assert_eq!(alice.to_json(), after);
}

#[test]
fn a_transfer_without_change_stays_outgoing() {
    let mut mint = issuer(4);
    let (mut alice, mut bob, carol) = (Holder::new(), Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[4]);
    let request = bob.request(&view(&mint), 4).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let transfer = &payment.transfers[0].request;
    let answer = mint.transfer(transfer).unwrap();
    assert_eq!(bob.absorb(&answer), 4);
    assert!(bob.theirs(&answer).is_empty());
    assert_eq!(carol.theirs(&answer), answer);
    assert_eq!(carol.at_stake(transfer), None);

    // Only a resend shows Alice it was applied.
    assert_eq!(alice.take_answers(&answer), 0);
    assert_eq!(alice.outgoing().count(), 1);
    assert_eq!(alice.at_stake(transfer), Some(0));
    assert_eq!(alice.settle(transfer, &mint.transfer(transfer).unwrap()), 0);
    assert_eq!(alice.outgoing().count(), 0);
    assert_eq!(alice.at_stake(transfer), None);
}

#[test]
fn transfers_are_due_for_a_resend_while_the_next_rotation_would_cut_them_off() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    // Minted second to last, the notes die in three rotations.
    fund(&mut mint, &mut alice, &[8, 8]);
    let request = bob.request(&view(&mint), 3).unwrap();
    let early = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    assert!(alice.due_resends(&view(&mint), 0).is_empty());
    mint.rotate().unwrap();
    mint.rotate().unwrap();

    // The note the payment spends dies at the next rotation, so it is due.
    let before = alice.to_json();
    let due = alice.due_resends(&view(&mint), 0);
    assert_eq!(alice.to_json(), before, "nothing in the store changes");
    let [resend] = due.as_slice() else {
        panic!("{due:?}")
    };
    assert_eq!(resend.request, early.transfers[0].request);
    assert!(resend.after_secs <= 60);
    // A refresh, once planned, is due as well: plan the resends first.
    assert_eq!(alice.refresh(&view(&mint), 0).len(), 1);
    assert_eq!(alice.due_resends(&view(&mint), 0).len(), 2);

    // Bob submits; after the rotation that drops the spent note, the mint
    // still answers a resend, so it is still due until the next.
    let answer = mint.transfer(&early.transfers[0].request).unwrap();
    assert_eq!(bob.absorb(&answer), 3);
    mint.rotate().unwrap();
    let due = alice.due_resends(&view(&mint), 0);
    assert!(due.iter().any(|t| t.request == early.transfers[0].request));
    assert_eq!(mint.transfer(&early.transfers[0].request).unwrap(), answer);
    mint.rotate().unwrap();
    assert!(alice.due_resends(&view(&mint), 0).is_empty());

    // Within a schedule, they are due in refresh's window.
    let mut alice = Holder::new();
    fund(&mut mint, &mut alice, &[8]);
    mint.rotate().unwrap();
    mint.rotate().unwrap();
    let request = bob.request(&view(&mint), 3).unwrap();
    alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let mut scheduled = view(&mint);
    scheduled.schedule = Some(Schedule {
        period_secs: 3_600,
        next_at: 2_000,
    });
    let due = alice.due_resends(&scheduled, 1_000);
    assert_eq!(due.len(), 1);
    assert!(due[0].after_secs <= 700, "{}", due[0].after_secs);
}

#[test]
fn resending_spaces_the_outgoing_transfers() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[2, 2, 2, 2]);
    let request = bob.request_at_most(&view(&mint), 8, 2).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    assert_eq!(payment.transfers.len(), 4);

    let spacing = Spacing {
        min_secs: 5,
        max_secs: 9,
    };
    let resent = alice.resend(spacing);
    let mut sent: Vec<_> = resent.iter().map(|t| t.request.clone()).collect();
    let mut outgoing: Vec<_> = alice.outgoing().cloned().collect();
    sent.sort_by_key(|r| r.inputs[0].note.serial());
    outgoing.sort_by_key(|r| r.inputs[0].note.serial());
    assert_eq!(sent, outgoing);
    assert_eq!(resent[0].after_secs, 0);
    for pair in resent.windows(2) {
        let gap = pair[1].after_secs - pair[0].after_secs;
        assert!((5..=9).contains(&gap), "gap {gap}");
    }
}

#[test]
fn a_saved_store_spends_after_loading() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[2, 16]);
    let mut alice = Holder::from_json(&alice.to_json()).unwrap();
    let request = bob.request(&view(&mint), 16).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let answers = submit(&mut mint, &payment);
    assert_eq!(bob.absorb(&answers[0]), 16);

    assert!(Holder::from_json("{}").is_err());
    let newer = alice.to_json().replace("\"format\":1", "\"format\":9");
    assert!(matches!(
        Holder::from_json(&newer),
        Err(worgl_sdk::LoadError::Format(9))
    ));
}

#[test]
fn cancelling_returns_the_note() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let request = bob.request(&view(&mint), 3).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    assert_eq!(alice.balance(&view(&mint)).spendable, 0);
    assert!(alice.cancel(&payment.transfers[0].request));
    assert!(!alice.cancel(&payment.transfers[0].request));
    let balance = alice.balance(&view(&mint));
    // The change keys stay, in case the mint applied it after all.
    assert_eq!(
        (
            balance.spendable,
            balance.outgoing,
            balance.expected,
            balance.unconfirmed
        ),
        (8, 0, 0, 5)
    );
    assert_eq!(alice.payments().count(), 0);
}

#[test]
fn a_balance_adds_up_through_a_payment_a_refresh_and_a_cancel() {
    let mut mint = issuer(4);
    let (mut alice, mut bob, mut carol) = (Holder::new(), Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8, 4]);
    assert_eq!(parts(&balance(&alice, &mint)), [12, 0, 0, 0, 0, 0]);

    // Bob asks for 3, which is not his yet.
    let request = bob.request(&view(&mint), 3).unwrap();
    let b = balance(&bob, &mint);
    assert_eq!(parts(&b), [0, 0, 0, 0, 3, 0]);
    assert_eq!(b.total(), 0);

    // Alice pays from her 4: 3 is on its way to Bob, 1 back to her.
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let b = balance(&alice, &mint);
    assert_eq!(parts(&b), [8, 3, 1, 0, 0, 0]);
    assert_eq!((b.total(), b.outgoing, b.expected), (9, 4, 1));

    // She submits it: her change is back, and Bob's 3 is his once handed over.
    let answers = submit(&mut mint, &payment);
    settle_all(&mut alice, &payment, &answers);
    let b = balance(&alice, &mint);
    assert_eq!(parts(&b), [9, 0, 0, 0, 0, 0]);
    assert_eq!((b.frozen, b.total(), b.outgoing), (1, 9, 0));
    assert_eq!(parts(&balance(&bob, &mint)), [0, 0, 0, 0, 3, 0]);
    let owed = alice.payments().next().unwrap().owed.clone();
    assert_eq!(bob.take_answers(&owed), 3);
    let b = balance(&bob, &mint);
    assert_eq!(parts(&b), [3, 0, 0, 0, 0, 0]);
    assert_eq!(b.total(), 3);

    // The 8 is about to die; refreshed, it is on its way back to her.
    mint.rotate().unwrap();
    mint.rotate().unwrap();
    let refreshes = alice.refresh(&view(&mint), 0);
    assert_eq!(refreshes.len(), 1);
    let b = balance(&alice, &mint);
    assert_eq!(parts(&b), [1, 0, 0, 8, 0, 0]);
    assert_eq!((b.total(), b.outgoing, b.expected), (9, 8, 8));
    let answer = mint.transfer(&refreshes[0].request).unwrap();
    assert_eq!(alice.settle(&refreshes[0].request, &answer), 8);
    assert_eq!(parts(&balance(&alice, &mint)), [9, 0, 0, 0, 0, 0]);

    // She pays Carol 2 from the 8, then cancels: the 8 is back, and its
    // change is unconfirmed, outside the total.
    let request = carol.request(&view(&mint), 2).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let b = balance(&alice, &mint);
    assert_eq!(parts(&b), [1, 2, 6, 0, 0, 0]);
    assert_eq!(b.total(), 7);
    assert!(alice.cancel(&payment.transfers[0].request));
    let b = balance(&alice, &mint);
    assert_eq!(parts(&b), [9, 0, 0, 0, 0, 6]);
    assert_eq!((b.total(), b.outgoing, b.expected), (9, 0, 0));
    assert_eq!(parts(&balance(&carol, &mint)), [0, 0, 0, 0, 2, 0]);
}

#[test]
fn a_cancelled_transfers_change_comes_only_in_place_of_its_note() {
    let mut mint = issuer(4);
    let (mut alice, mut bob, mut carol) = (Holder::new(), Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let request = bob.request(&view(&mint), 2).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    assert!(alice.cancel(&payment.transfers[0].request));
    let b = balance(&alice, &mint);
    assert_eq!(parts(&b), [8, 0, 0, 0, 0, 6]);
    assert_eq!(b.total(), 8);

    // Bob submits it after all. The mint applies it by spending the 8 taken
    // back, so the 6 comes in its place.
    let eight = *alice.notes().next().unwrap();
    let answer = mint.transfer(&payment.transfers[0].request).unwrap();
    assert!(matches!(mint.status(&eight), Ok(worgl::NoteStatus::Spent)));
    assert_eq!(alice.absorb(&answer), 6);

    // Alice counts the 8 until the mint refuses it, as any note spent
    // elsewhere; then she has 6, never the 14 held and unconfirmed added up
    // to.
    let request = carol.request(&view(&mint), 8).unwrap();
    let refused = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let [transfer] = refused.transfers.as_slice() else {
        panic!("8 is paid from the 8 alone");
    };
    assert!(matches!(
        mint.transfer(&transfer.request),
        Err(worgl::Error::Spent(_))
    ));
    assert!(alice.forget(&transfer.request));
    let b = balance(&alice, &mint);
    assert_eq!(parts(&b), [6, 0, 0, 0, 0, 0]);
    assert_eq!(b.total(), 6);
}

#[test]
fn a_transfer_cancelled_or_forgotten_leaves_its_payment() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[4, 4]);
    let request = bob.request(&view(&mint), 6).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let [first, second] = &payment.transfers[..] else {
        panic!("{payment:?}")
    };

    // Never handed out again with the payment, which no longer pays the
    // whole request.
    assert!(alice.cancel(&first.request));
    let [made] = alice.payments().collect::<Vec<_>>()[..] else {
        panic!("the payment is kept while a transfer is outgoing")
    };
    assert_eq!(made.payment.transfers, std::slice::from_ref(second));
    assert!(matches!(
        made.request.check(&made.payment),
        Err(CheckError::Missing(_))
    ));
    assert_eq!(alice.balance(&view(&mint)).spendable, 4);
    let reloaded = Holder::from_json(&alice.to_json()).unwrap();
    assert_eq!(reloaded.payments().next().unwrap().payment, made.payment);

    assert!(alice.forget(&second.request));
    assert_eq!(alice.payments().count(), 0);
}

/// A payee's request, by hand, for notes of `amounts` to fresh keys.
fn asking(mint: &Issuer, amounts: &[u64]) -> PaymentRequest {
    PaymentRequest {
        mint: *mint.public_key(),
        outputs: amounts
            .iter()
            .map(|&amount| Output {
                amount,
                holder: HolderKey::generate().public_key(),
            })
            .collect(),
    }
}

#[test]
fn a_request_for_notes_the_mint_would_not_make_is_refused_before_signing() {
    let mut mint = issuer(4);
    let mut alice = Holder::new();
    fund(&mut mint, &mut alice, &[1024, 512]);
    let before = alice.to_json();
    for (amounts, refused) in [
        (vec![3], 3),
        (vec![0], 0),
        (vec![511, 511, 511], 511),
        (vec![4, 6], 6),
    ] {
        let request = asking(&mint, &amounts);
        assert_eq!(
            alice.pay(&view(&mint), &request, Spacing::default()),
            Err(PayError::Denomination(refused))
        );
        assert_eq!(alice.to_json(), before, "nothing changes");
    }
    let balance = alice.balance(&view(&mint));
    assert_eq!((balance.spendable, balance.outgoing), (1536, 0));
}

#[test]
fn a_request_already_paid_is_refused() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8, 8]);
    let request = bob.request(&view(&mint), 3).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let now = view(&mint);
    let paid_twice = |alice: &mut Holder| {
        let before = alice.to_json();
        let error = alice.pay(&now, &request, Spacing::default()).unwrap_err();
        assert!(
            matches!(error, PayError::Paid(key) if request.outputs.iter().any(|o| o.holder == key)),
            "{error}"
        );
        assert_eq!(alice.to_json(), before);
    };
    // While the transfer is outgoing.
    paid_twice(&mut alice);

    // Once it is done with, among the keys paid last, even after a reload.
    let answer = mint.transfer(&payment.transfers[0].request).unwrap();
    bob.absorb(&answer);
    alice.take_answers(&bob.theirs(&answer));
    assert_eq!(alice.outgoing().count(), 0);
    assert_eq!(alice.payments().count(), 0);
    paid_twice(&mut alice);
    let mut alice = Holder::from_json(&alice.to_json()).unwrap();
    paid_twice(&mut alice);

    // A request asking twice for one key is refused too.
    let mut twice = asking(&mint, &[1, 2]);
    twice.outputs[1].holder = twice.outputs[0].holder;
    assert_eq!(
        alice.pay(&view(&mint), &twice, Spacing::default()),
        Err(PayError::Paid(twice.outputs[0].holder))
    );
    // A new request is paid.
    let again = bob.request(&view(&mint), 3).unwrap();
    assert!(alice.pay(&view(&mint), &again, Spacing::default()).is_ok());
}

#[test]
fn a_dropped_transfer_still_takes_in_its_change() {
    // Two receipts stand in for a mint whose receipts many transfers pushed out.
    let mut policy = Policy::cash_like();
    policy.receipt_capacity = 2;
    let mut mint = issuer_with(4, policy);
    let (mut alice, mut bob, mut carol) = (Holder::new(), Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    fund(&mut mint, &mut carol, &[1, 1, 1]);
    let request = bob.request(&view(&mint), 3).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let transfer = &payment.transfers[0].request;
    let answer = mint.transfer(transfer).unwrap();
    bob.absorb(&answer);
    for _ in 0..3 {
        let paid = Holder::new().request(&view(&mint), 1).unwrap();
        let payment = carol.pay(&view(&mint), &paid, Spacing::default()).unwrap();
        mint.transfer(&payment.transfers[0].request).unwrap();
    }
    assert!(matches!(
        mint.transfer(transfer),
        Err(worgl::Error::Spent(_))
    ));

    // Refused as spent: the note is gone, so the transfer is dropped.
    assert!(alice.forget(transfer));
    assert!(!alice.forget(transfer));
    let balance = alice.balance(&view(&mint));
    assert_eq!(
        (balance.outgoing, balance.expected, balance.unconfirmed),
        (0, 0, 5)
    );
    assert_eq!(alice.payments().count(), 0);
    assert_eq!(alice.at_stake(transfer), Some(5));
    // The change keys are this holder's, not the counterparty's.
    assert!(
        alice
            .theirs(&answer)
            .iter()
            .all(|n| request.outputs.iter().any(|o| o.holder == *n.holder()))
    );

    // Kept across a save, in format 2, and taken in once handed back.
    let json = alice.to_json();
    assert!(json.contains("\"format\":2"), "{json}");
    let mut alice = Holder::from_json(&json).unwrap();
    assert_eq!(alice.take_answers(&bob.theirs(&answer)), 5);
    let balance = alice.balance(&view(&mint));
    assert_eq!((balance.frozen, balance.unconfirmed), (5, 0));
    assert!(alice.to_json().contains("\"format\":1"));
}

#[test]
fn a_resend_that_carries_out_a_payment_keeps_the_payees_notes() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let request = bob.request(&view(&mint), 5).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let [made] = alice.payments().collect::<Vec<_>>()[..] else {
        panic!("one payment is kept")
    };
    assert_eq!((&made.request, &made.payment), (&request, &payment));
    assert!(made.owed.is_empty());

    // Bob never submits; Alice's resend carries the payment out.
    let resend = alice.resend(Spacing::default());
    let answer = mint.transfer(&resend[0].request).unwrap();
    assert_eq!(alice.settle(&resend[0].request, &answer), 3);
    assert_eq!(alice.outgoing().count(), 0);
    let owed = alice.payments().next().unwrap().owed.clone();
    assert_eq!(owed, alice.theirs(&answer));
    assert_eq!(owed.len(), request.outputs.len());
    // Settling the same answer again keeps nothing twice.
    alice.settle(&resend[0].request, &answer);
    assert_eq!(alice.payments().next().unwrap().owed, owed);
    let json = alice.to_json();
    assert!(json.contains("\"format\":2"), "{json}");
    let mut alice = Holder::from_json(&json).unwrap();

    // Handed to Bob, who checks and keeps them; Alice keeps them no more.
    assert!(owed.iter().all(|n| n.verify(mint.keys()).is_ok()));
    assert_eq!(bob.take_answers(&owed), 5);
    assert_eq!(alice.forget_owed(&owed), 5);
    assert_eq!(alice.payments().count(), 0);
    assert_eq!(alice.forget_owed(&owed), 0);
}

#[test]
fn pruning_drops_change_and_owed_notes_that_can_no_longer_arrive() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8, 8]);
    // One payment carried out by Alice's resend, one forgotten.
    let request = bob.request(&view(&mint), 5).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let transfer = &payment.transfers[0].request;
    alice.settle(transfer, &mint.transfer(transfer).unwrap());
    let request = bob.request(&view(&mint), 5).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    alice.forget(&payment.transfers[0].request);
    assert_eq!(alice.balance(&view(&mint)).unconfirmed, 3);

    // The notes and change sit one layer past the spent notes: the last.
    for _ in 0..3 {
        mint.rotate().unwrap();
        assert_eq!(alice.prune(&view(&mint)), 0);
        assert_eq!(alice.payments().count(), 1);
        assert_eq!(alice.balance(&view(&mint)).unconfirmed, 3);
    }
    mint.rotate().unwrap();
    assert_eq!(alice.prune(&view(&mint)), 3, "the change taken in expired");
    assert_eq!(alice.payments().count(), 0);
    assert_eq!(alice.balance(&view(&mint)).unconfirmed, 0);
}

#[test]
fn a_store_of_format_1_loads() {
    let mut mint = issuer(4);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let request = bob.request(&view(&mint), 3).unwrap();
    alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    // As the first release wrote it: no payments, nothing paid.
    let mut saved: serde_json::Value = serde_json::from_str(&alice.to_json()).unwrap();
    assert_eq!(saved["format"], 1);
    let fields = saved.as_object_mut().unwrap();
    for field in ["unconfirmed", "payments", "paid"] {
        assert!(fields.remove(field).is_some(), "{field}");
    }
    let old = Holder::from_json(&saved.to_string()).unwrap();
    assert_eq!(old.balance(&view(&mint)), alice.balance(&view(&mint)));
    assert_eq!(parts(&balance(&old, &mint)), [0, 3, 5, 0, 0, 0]);
    assert_eq!(old.outgoing().count(), 1);
    assert_eq!(old.payments().count(), 0);
}

#[test]
fn a_view_counts_down_to_the_next_rotation_and_no_further() {
    let mint = issuer(4);
    let mut v = view(&mint);
    v.schedule = Some(Schedule {
        period_secs: 100,
        next_at: 1_000,
    });
    assert_eq!(v.until_rotation(990), Some(10));
    // Past its time, the rotation is still to come as far as the view
    // knows, however far the clock has gone.
    assert_eq!(v.until_rotation(1_000), Some(0));
    assert_eq!(v.until_rotation(1_250), Some(0));

    v.paused = true;
    assert_eq!(v.until_rotation(5_000), None);
}

#[test]
fn a_view_tells_when_a_note_expires() {
    let mut mint = issuer(4);
    let mut alice = Holder::new();
    fund(&mut mint, &mut alice, &[8]);
    let note = *alice.notes().next().unwrap();
    let mut v = view(&mint);
    assert_eq!(v.expires_at(&note), None);

    // Second to last of four layers: dropped by the third rotation.
    let schedule = Schedule {
        period_secs: 100,
        next_at: 1_000,
    };
    v.schedule = Some(schedule);
    assert_eq!(v.expires_at(&note), Some(1_200));

    let mut other = issuer(4);
    let stranger = other
        .mint(8, worgl::HolderKey::generate().public_key())
        .unwrap();
    assert_eq!(v.expires_at(&stranger), None);
    v.paused = true;
    assert_eq!(v.expires_at(&note), None);

    // Once the mint has made those rotations, its info says the note is gone.
    for _ in 0..3 {
        mint.rotate().unwrap();
    }
    let mut later = view(&mint);
    later.schedule = Some(Schedule {
        next_at: 1_300,
        ..schedule
    });
    assert_eq!(later.expires_at(&note), None);
}

#[test]
fn a_refresh_planned_past_the_rotations_time_is_due_at_once() {
    let mut mint = issuer(3);
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    // Minted into position 1 of 3, the notes are in the first layer after a
    // rotation and die at the next.
    fund(&mut mint, &mut alice, &[2, 4]);
    mint.rotate().unwrap();
    let request = bob.request(&view(&mint), 2).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    let mut v = view(&mint);
    v.schedule = Some(Schedule {
        period_secs: 3_600,
        next_at: 10_000,
    });

    // Ahead of it, they are spread over the time left, less the margin.
    let mut early = alice.clone();
    let resends = early.due_resends(&v, 9_000);
    let refreshes = early.refresh(&v, 9_000);
    assert_eq!((resends.len(), refreshes.len()), (1, 1));
    let window = 1_000 - REFRESH_MARGIN_SECS;
    assert!(
        resends
            .iter()
            .chain(&refreshes)
            .all(|t| t.after_secs <= window)
    );

    // At or past its time, by a clock ahead of the mint's or because the
    // mint has not rotated yet, the first layer still dies at the next
    // rotation: everything is due at once.
    for now in [10_000, 10_000 + 5 * 3_600 + 1] {
        let mut late = alice.clone();
        let resends = late.due_resends(&v, now);
        let refreshes = late.refresh(&v, now);
        assert_eq!((resends.len(), refreshes.len()), (1, 1), "{now}");
        assert!(
            resends.iter().chain(&refreshes).all(|t| t.after_secs == 0),
            "{now}"
        );
        assert_eq!(resends[0].request, payment.transfers[0].request);
    }
    // The mint has not rotated, so it takes them.
    let refreshes = alice.refresh(&v, 10_000);
    let answer = mint.transfer(&refreshes[0].request).unwrap();
    assert_eq!(alice.settle(&refreshes[0].request, &answer), 4);
    assert_eq!(
        mint.transfer(&payment.transfers[0].request).unwrap().len(),
        1
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any holding and any amount: either the payment goes through the
    /// issuer and moves exactly the amount, or it fails and changes nothing.
    #[test]
    fn payments_move_exactly_the_amount_or_nothing(
        bits in prop::collection::vec(0u32..10, 1..12),
        amount in 1u64..3000,
    ) {
        let mut mint = issuer(4);
        let (mut alice, mut bob) = (Holder::new(), Holder::new());
        let notes: Vec<u64> = bits.iter().map(|&b| 1 << b).collect();
        fund(&mut mint, &mut alice, &notes);
        let held: u128 = notes.iter().map(|&n| u128::from(n)).sum();
        let before = alice.to_json();

        let request = bob.request(&view(&mint), amount).unwrap();
        match alice.pay(&view(&mint), &request, Spacing::default()) {
            Ok(payment) => {
                prop_assert_eq!(request.check(&payment), Ok(()));
                // The amount is on its way to Bob, and the rest is Alice's.
                let b = balance(&alice, &mint);
                prop_assert_eq!(b.sending, u128::from(amount));
                prop_assert_eq!(b.outgoing, b.sending + b.change);
                prop_assert_eq!(b.total(), held - u128::from(amount));
                let mut keys = HashSet::new();
                for t in &payment.transfers {
                    prop_assert_eq!(t.request.inputs.len(), 1);
                    for o in &t.request.outputs {
                        prop_assert!(keys.insert(o.holder), "a key was used twice");
                    }
                }
                let answers = submit(&mut mint, &payment);
                let received: u128 = answers.iter().map(|n| bob.absorb(n)).sum();
                prop_assert_eq!(received, u128::from(amount));
                settle_all(&mut alice, &payment, &answers);
                prop_assert_eq!(balance(&alice, &mint).held(), held - u128::from(amount));
                prop_assert_eq!(alice.outgoing().count(), 0);
            }
            Err(PayError::Unaffordable { spendable, needed }) => {
                prop_assert_eq!((spendable, needed), (held, u128::from(amount)));
                prop_assert!(held < u128::from(amount));
                prop_assert_eq!(alice.to_json(), before);
            }
            Err(error) => {
                prop_assert!(held >= u128::from(amount));
                prop_assert!(
                    matches!(error, PayError::Fragmented { .. } | PayError::TooManyOutputs { .. }),
                    "{error}"
                );
                prop_assert_eq!(alice.to_json(), before);
            }
        }
    }
}
