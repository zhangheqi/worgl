//! Merging two copies of one purse, against a real issuer in memory.

use worgl::{Issuer, IssuerKey, LedgerConfig, Note, NoteStatus, Policy, TransferRequest};
use worgl_sdk::{
    Balance, DISCARDED_NOTES_KEPT, ENDED_TRANSFERS_KEPT, Holder, Merged, MintInfo, MintView,
    Spacing,
};

fn issuer() -> Issuer {
    let ledger = LedgerConfig {
        layers: 4,
        layer_size: 4096,
    };
    Issuer::new(IssuerKey::generate(), ledger, Policy::cash_like()).unwrap()
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
        let note = issuer.mint(amount, request.outputs[0].holder).unwrap();
        assert_eq!(holder.absorb(&[note]), u128::from(amount));
    }
}

/// A copy of `holder` as its saved JSON gives it back: a backup.
fn copy(holder: &Holder) -> Holder {
    Holder::from_json(&holder.to_json()).unwrap()
}

/// A copy of `holder` as a release that kept no record of the transfers it
/// ended saved it.
fn without_record(holder: &Holder) -> Holder {
    let mut saved: serde_json::Value = serde_json::from_str(&holder.to_json()).unwrap();
    saved.as_object_mut().unwrap().remove("ended");
    Holder::from_json(&saved.to_string()).unwrap()
}

/// `holder` with `other` merged in, and what the merge said.
fn merging(holder: &Holder, other: &Holder) -> (Holder, Merged) {
    let mut holder = copy(holder);
    let report = holder.merge(other);
    (holder, report)
}

/// Pays `amount` from `payer` to a fresh request of `payee`, and returns the
/// one transfer.
fn pay(issuer: &Issuer, payer: &mut Holder, payee: &mut Holder, amount: u64) -> TransferRequest {
    let request = payee.request(&view(issuer), amount).unwrap();
    let payment = payer
        .pay(&view(issuer), &request, Spacing::default())
        .unwrap();
    assert_eq!(payment.transfers.len(), 1);
    payment.transfers[0].request.clone()
}

/// Submits `transfer` as its payer, settles the answer and hands the
/// payee's notes over, as a payer that submits its own payment does.
fn complete(issuer: &mut Issuer, payer: &mut Holder, transfer: &TransferRequest) {
    let answer = issuer.transfer(transfer).unwrap();
    payer.settle(issuer.keys(), transfer, &answer).unwrap();
    let owed: Vec<Note> = payer.payments().flat_map(|p| p.owed.clone()).collect();
    payer.forget_owed(&owed);
    assert_eq!(payer.payments().count(), 0);
}

/// Resends every outgoing transfer of `holder`, settling the answers and
/// forgetting what the mint refuses, as a purse's settle does.
fn resend(issuer: &mut Issuer, holder: &mut Holder) {
    for scheduled in holder.resend(Spacing::default()) {
        match issuer.transfer(&scheduled.request) {
            Ok(answer) => {
                holder
                    .settle(issuer.keys(), &scheduled.request, &answer)
                    .unwrap();
            }
            Err(_) => assert!(holder.forget(&scheduled.request)),
        }
    }
}

/// Asks the mint about each unchecked note, discarding those it reports
/// spent or expired and keeping the rest, and returns the value discarded.
fn check(issuer: &Issuer, holder: &mut Holder) -> u128 {
    let (gone, live): (Vec<Note>, Vec<Note>) = holder.unchecked().partition(|note| {
        matches!(
            issuer.status(note).unwrap(),
            NoteStatus::Spent | NoteStatus::Expired
        )
    });
    holder.keep(&live);
    holder.discard(&gone)
}

/// A holder's balance at `issuer`, whose sums are checked against its parts.
fn balance(holder: &Holder, issuer: &Issuer) -> Balance {
    let b = holder.balance(&view(issuer));
    assert_eq!(b.expected, b.requested + b.change + b.refreshing, "{b:?}");
    assert!(b.unchecked <= b.total(), "{b:?}");
    b
}

/// The balance, leaving out how much of it is unchecked.
fn counted(holder: &Holder, issuer: &Issuer) -> Balance {
    Balance {
        unchecked: 0,
        ..balance(holder, issuer)
    }
}

#[test]
fn a_copy_merged_into_itself_changes_nothing() {
    let mut mint = issuer();
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8, 4]);
    pay(&mint, &mut alice, &mut bob, 5);
    alice.request(&view(&mint), 3).unwrap();

    let (merged, report) = merging(&alice, &copy(&alice));
    assert_eq!(report, Merged::default());
    assert_eq!(merged.to_json(), alice.to_json());
}

#[test]
fn a_note_in_both_copies_is_held_once() {
    let mut mint = issuer();
    let mut alice = Holder::new();
    fund(&mut mint, &mut alice, &[8]);
    let backup = copy(&alice);
    fund(&mut mint, &mut alice, &[4]);

    // Restored into an empty purse, the backup is all new, and unchecked.
    let (fresh, report) = merging(&Holder::new(), &backup);
    assert_eq!(report.notes, backup.notes().copied().collect::<Vec<_>>());
    assert_eq!(fresh.unchecked().copied().collect::<Vec<_>>(), report.notes);
    assert_eq!(counted(&fresh, &mint), balance(&backup, &mint));
    assert_eq!(balance(&fresh, &mint).unchecked, 8);

    let (merged, report) = merging(&alice, &backup);
    assert!(report.notes.is_empty(), "{report:?}");
    assert_eq!(merged.notes().count(), 2);
    assert_eq!(balance(&merged, &mint), balance(&alice, &mint));
    // The other way round, the backup takes in what came after it.
    let (merged, report) = merging(&backup, &alice);
    assert_eq!(
        report.notes.iter().map(Note::amount).collect::<Vec<_>>(),
        [4]
    );
    assert_eq!(counted(&merged, &mint), balance(&alice, &mint));
    // A note one copy doubts stays doubted, whatever another copy says,
    // which may be out of date: the same backup restored again vouches for
    // nothing. One only the other copy has is unchecked.
    let (merged, _) = merging(&fresh, &alice);
    assert_eq!(
        merged.unchecked().map(Note::amount).collect::<Vec<_>>(),
        [8, 4]
    );
    let (again, report) = merging(&fresh, &backup);
    assert_eq!(report, Merged::default());
    assert_eq!(again.unchecked().count(), 1);
    // A part as current, joined, vouches for what it does not doubt.
    let mut joined = copy(&fresh);
    joined.join(&alice);
    assert_eq!(joined.unchecked().count(), 0);
}

#[test]
fn a_note_one_copy_spends_waits_with_the_transfer() {
    let mut mint = issuer();
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let backup = copy(&alice);
    let transfer = pay(&mint, &mut alice, &mut bob, 5);
    let paying = balance(&alice, &mint);
    assert_eq!((paying.sending, paying.change, paying.total()), (5, 3, 3));

    // The backup's 8 is the note the transfer spends: it is not held twice.
    let (merged, report) = merging(&alice, &backup);
    assert_eq!(report, Merged::default());
    assert_eq!(balance(&merged, &mint), paying);
    // Merged into the backup, the transfer comes along and takes the note.
    // Its change is unchecked: the backup knew nothing of it.
    let (merged, report) = merging(&backup, &alice);
    assert_eq!(report.outgoing, [transfer]);
    assert!(report.notes.is_empty());
    assert_eq!(counted(&merged, &mint), paying);
    assert_eq!(balance(&merged, &mint).unchecked, 3);
    assert_eq!(merged.notes().count(), 0);
}

#[test]
fn a_transfer_the_other_copy_settled_is_settled() {
    let mut mint = issuer();
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let transfer = pay(&mint, &mut alice, &mut bob, 5);
    let sending = copy(&alice);
    let answer = mint.transfer(&transfer).unwrap();
    assert_eq!(alice.settle(mint.keys(), &transfer, &answer).unwrap(), 3);
    let settled = balance(&alice, &mint);

    // The copy that sent it learns that the mint answered: alice holds the
    // change, paid to a key that copy expects, so nothing counts twice and
    // nothing is unchecked.
    let (merged, report) = merging(&sending, &alice);
    assert_eq!(report.ended, std::slice::from_ref(&transfer));
    assert_eq!(
        report
            .notes
            .iter()
            .map(|n| u128::from(n.amount()))
            .sum::<u128>(),
        3
    );
    assert!(report.outgoing.is_empty());
    assert_eq!(balance(&merged, &mint), settled);
    assert_eq!(merged.outgoing().count(), 0);
    // And alice, merging the copy in, keeps what she has.
    let (merged, report) = merging(&alice, &sending);
    assert_eq!(report, Merged::default());
    assert_eq!(balance(&merged, &mint), settled);

    // Spent again since, in a transfer not yet answered, the first still
    // ended.
    mint.rotate().unwrap();
    let again = pay(&mint, &mut alice, &mut bob, 2);
    let (merged, report) = merging(&sending, &alice);
    assert_eq!(report.ended, [transfer]);
    assert_eq!(report.outgoing, [again]);
    assert_eq!(counted(&merged, &mint), balance(&alice, &mint));
}

#[test]
fn a_transfer_settled_and_its_change_spent_since_does_not_come_back() {
    let mut mint = issuer();
    let (mut alice, mut bob, mut carol) = (Holder::new(), Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let to_bob = pay(&mint, &mut alice, &mut bob, 5);
    // A backup made while the payment was on its way.
    let backup = copy(&alice);
    complete(&mut mint, &mut alice, &to_bob);
    mint.rotate().unwrap();
    // The change is spent since, and that payment is done with too.
    let to_carol = pay(&mint, &mut alice, &mut carol, 2);
    complete(&mut mint, &mut alice, &to_carol);
    let now = balance(&alice, &mint);
    assert_eq!((now.total(), now.held()), (1, 1));

    // Alice remembers ending the transfer the backup has outgoing: it ends
    // here too, and its change, spent since, is not expected again.
    let (merged, report) = merging(&alice, &backup);
    assert_eq!(report, Merged::default());
    assert_eq!(balance(&merged, &mint), now);
    assert_eq!(merged.outgoing().count(), 0);
    // So, the other way round, does the backup.
    let (merged, report) = merging(&backup, &alice);
    assert_eq!(report.ended, [to_bob]);
    assert_eq!(counted(&merged, &mint), now);
}

#[test]
fn a_transfer_brought_back_pays_unchecked_notes_until_the_mint_says() {
    let mut mint = issuer();
    let (mut alice, mut bob, mut carol) = (Holder::new(), Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let to_bob = pay(&mint, &mut alice, &mut bob, 5);
    let backup = copy(&alice);
    complete(&mut mint, &mut alice, &to_bob);
    mint.rotate().unwrap();
    let to_carol = pay(&mint, &mut alice, &mut carol, 2);
    complete(&mut mint, &mut alice, &to_carol);
    let truth = balance(&alice, &mint);

    // Restored on a new device, or into a store saved without a record of
    // the transfers it ended, the backup's transfer comes back: nothing here
    // shows it settled.
    for into in [Holder::new(), without_record(&alice)] {
        let (mut merged, report) = merging(&into, &backup);
        assert_eq!(report.outgoing, std::slice::from_ref(&to_bob));
        let b = balance(&merged, &mint);
        assert!(b.change > 0);
        assert_eq!(b.unchecked, b.change);
        // The mint replays its answer, change and all, though alice spent
        // part of that change since: what it pays this holder is unchecked.
        resend(&mut mint, &mut merged);
        assert_eq!(merged.outgoing().count(), 0);
        assert!(
            merged
                .unchecked()
                .any(|note| { mint.status(note).unwrap() == NoteStatus::Spent })
        );
        // Asked, the mint finds the spent one, and the rest is kept.
        assert_eq!(check(&mint, &mut merged), 2);
        assert_eq!(merged.unchecked().count(), 0);
        let b = balance(&merged, &mint);
        assert_eq!((b.total(), b.held()), (truth.total(), 1));
    }
}

#[test]
fn a_transfer_the_other_copy_forgot_or_cancelled_ends_the_same_way() {
    let mut mint = issuer();
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let transfer = pay(&mint, &mut alice, &mut bob, 5);
    let sending = copy(&alice);

    // Refused as spent: forgotten, its change left unconfirmed.
    let mut forgot = copy(&alice);
    assert!(forgot.forget(&transfer));
    let (merged, report) = merging(&sending, &forgot);
    assert_eq!(report.ended, std::slice::from_ref(&transfer));
    assert_eq!(merged.outgoing().count(), 0);
    assert_eq!(balance(&merged, &mint), balance(&forgot, &mint));
    assert_eq!(balance(&merged, &mint).unconfirmed, 3);

    // Refused as layer_full: cancelled, the note held again.
    let mut cancelled = copy(&alice);
    assert!(cancelled.cancel(&transfer));
    let (merged, report) = merging(&sending, &cancelled);
    assert_eq!(report.ended, std::slice::from_ref(&transfer));
    assert_eq!(
        report.notes.iter().map(Note::amount).collect::<Vec<_>>(),
        [8]
    );
    assert_eq!(balance(&merged, &mint), balance(&cancelled, &mint));
    assert_eq!(merged.payments().count(), 0);
    // Merged the other way, the ending copy keeps its word.
    let (merged, report) = merging(&cancelled, &sending);
    assert_eq!(report, Merged::default());
    assert_eq!(balance(&merged, &mint), balance(&cancelled, &mint));

    // A store saved without a record of what it ended shows a transfer it
    // ended with change by the change it keeps unconfirmed.
    let (merged, report) = merging(&sending, &without_record(&cancelled));
    assert_eq!(report.ended, std::slice::from_ref(&transfer));
    assert_eq!(balance(&merged, &mint), balance(&cancelled, &mint));
}

#[test]
fn a_cancelled_transfer_with_no_change_stays_cancelled() {
    let mut mint = issuer();
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[4]);
    let transfer = pay(&mint, &mut alice, &mut bob, 4);
    assert_eq!(transfer.outputs.len(), 1, "the note pays bob whole");
    let backup = copy(&alice);
    // Refused as layer_full: cancelled, the note held again.
    assert!(alice.cancel(&transfer));
    let cancelled = balance(&alice, &mint);
    assert_eq!((cancelled.held(), cancelled.sending), (4, 0));

    // The backup's transfer is one alice ended: it is not sent again.
    let (merged, report) = merging(&alice, &backup);
    assert_eq!(report, Merged::default());
    assert_eq!(balance(&merged, &mint), cancelled);
    assert_eq!(merged.outgoing().count(), 0);
    let (merged, report) = merging(&backup, &alice);
    assert_eq!(report.ended, std::slice::from_ref(&transfer));
    assert_eq!(balance(&merged, &mint), cancelled);
    assert_eq!(merged.payments().count(), 0);
}

#[test]
fn a_note_the_other_copy_spent_is_dropped_or_left_out() {
    let mut mint = issuer();
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8, 2]);
    let backup = copy(&alice);
    // Alice pays from the 8, takes the change in and forgets the payment.
    let transfer = pay(&mint, &mut alice, &mut bob, 5);
    let answer = mint.transfer(&transfer).unwrap();
    let change = bob.theirs(&answer);
    alice.take_answers(&change);
    assert_eq!(alice.payments().count(), 0);
    assert_eq!(alice.outgoing().count(), 0);
    let now = balance(&alice, &mint);

    // Alice remembers spending the backup's 8: it is not taken back.
    let (merged, report) = merging(&alice, &backup);
    assert_eq!(report, Merged::default());
    assert_eq!(balance(&merged, &mint), now);
    // And the backup, merging alice in, drops it.
    let (merged, report) = merging(&backup, &alice);
    assert_eq!(
        report.spent.iter().map(Note::amount).collect::<Vec<_>>(),
        [8]
    );
    assert_eq!(counted(&merged, &mint), now);
}

#[test]
fn notes_a_backup_holds_that_were_spent_since_are_found_by_the_mint() {
    let mut mint = issuer();
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8, 2]);
    let backup = copy(&alice);
    let transfer = pay(&mint, &mut alice, &mut bob, 5);
    let answer = mint.transfer(&transfer).unwrap();
    let change = bob.theirs(&answer);
    alice.take_answers(&change);

    // Saved by a release that kept no record of what it ended, nothing in
    // either copy tells that the backup's 8 is spent.
    let (mut merged, report) = merging(&without_record(&alice), &backup);
    assert_eq!(
        report.notes.iter().map(Note::amount).collect::<Vec<_>>(),
        [8]
    );
    assert_eq!(
        merged.unchecked().copied().collect::<Vec<_>>(),
        report.notes
    );
    let b = balance(&merged, &mint);
    assert_eq!((b.total(), b.unchecked), (13, 8));
    // The mint does: drop what it reports spent or expired, keep the rest.
    assert_eq!(check(&mint, &mut merged), 8);
    assert_eq!(balance(&merged, &mint), balance(&alice, &mint));
}

#[test]
fn a_note_the_mint_reported_spent_stays_out_when_a_backup_is_merged_again() {
    let mut mint = issuer();
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8, 2]);
    let backup = copy(&alice);
    // Alice spends the 8 since.
    let transfer = pay(&mint, &mut alice, &mut bob, 5);
    mint.transfer(&transfer).unwrap();

    // A new purse restores the backup, and the mint reports the 8 spent.
    let (mut fresh, _) = merging(&Holder::new(), &backup);
    assert_eq!(check(&mint, &mut fresh), 8);
    let checked = balance(&fresh, &mint);
    assert_eq!((checked.total(), checked.unchecked), (2, 0));

    // Saved, it remembers: the same backup merged again gives nothing back.
    let (merged, report) = merging(&copy(&fresh), &backup);
    assert_eq!(report, Merged::default());
    assert_eq!(balance(&merged, &mint), checked);
    // And the backup, merging the checked purse in, drops the 8.
    let (merged, report) = merging(&backup, &fresh);
    assert_eq!(
        report.spent.iter().map(Note::amount).collect::<Vec<_>>(),
        [8]
    );
    assert_eq!(counted(&merged, &mint), counted(&fresh, &mint));
}

#[test]
fn notes_the_mint_reports_live_are_kept() {
    let mut mint = issuer();
    let mut alice = Holder::new();
    fund(&mut mint, &mut alice, &[8, 4]);
    let (mut fresh, report) = merging(&Holder::new(), &alice);
    assert_eq!(fresh.unchecked().count(), 2);
    assert_eq!(check(&mint, &mut fresh), 0);
    assert_eq!(fresh.unchecked().count(), 0);
    assert_eq!(balance(&fresh, &mint), balance(&alice, &mint));
    // The flag is saved with the store, and the notes of the answer to a
    // transfer that spends an unchecked note are checked.
    let (fresh, _) = merging(&Holder::new(), &alice);
    let mut fresh = copy(&fresh);
    assert_eq!(fresh.unchecked().count(), report.notes.len());
    let mut bob = Holder::new();
    let transfer = pay(&mint, &mut fresh, &mut bob, 3);
    let answer = mint.transfer(&transfer).unwrap();
    fresh.settle(mint.keys(), &transfer, &answer).unwrap();
    let eight: Vec<Note> = fresh.unchecked().copied().collect();
    assert_eq!(eight.iter().map(Note::amount).collect::<Vec<_>>(), [8]);
    assert_eq!(fresh.keep(&eight), 8);
    assert_eq!(fresh.unchecked().count(), 0);
}

#[test]
fn requests_from_either_copy_are_expected_until_paid() {
    let mut mint = issuer();
    let (mut alice, mut carol) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut carol, &[16, 1]);
    let backup = copy(&alice);
    let first = alice.request(&view(&mint), 4).unwrap();
    let mut device = copy(&backup);
    device.request(&view(&mint), 2).unwrap();

    let (merged, _) = merging(&alice, &device);
    assert_eq!(balance(&merged, &mint).requested, 6);

    // A request one copy saw paid is paid in the merge, not still asked.
    let payment = carol.pay(&view(&mint), &first, Spacing::default()).unwrap();
    let answer = mint.transfer(&payment.transfers[0].request).unwrap();
    assert_eq!(alice.absorb(&answer), 4);
    let (merged, report) = merging(&copy(&merged), &alice);
    assert_eq!(report.notes.len(), 1);
    let b = balance(&merged, &mint);
    assert_eq!((b.held(), b.requested, b.unchecked), (4, 2, 0));

    // A request one copy withdrew is expected again: a payer may still pay.
    let open = device.request(&view(&mint), 1).unwrap();
    let mut withdrawn = copy(&device);
    withdrawn.withdraw(&open);
    assert_eq!(balance(&withdrawn, &mint).requested, 2);
    let (mut merged, _) = merging(&withdrawn, &device);
    assert_eq!(balance(&merged, &mint).requested, 3);
    // Its note is unchecked when it comes: the other copy may have been paid
    // it and spent it.
    let payment = carol.pay(&view(&mint), &open, Spacing::default()).unwrap();
    let answer = mint.transfer(&payment.transfers[0].request).unwrap();
    assert_eq!(merged.absorb(&answer), 1);
    assert_eq!(merged.unchecked().count(), 1);
}

#[test]
fn payments_of_one_pay_call_are_one_with_the_notes_either_keeps() {
    let mut mint = issuer();
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[4, 4]);
    let request = bob.request(&view(&mint), 6).unwrap();
    let payment = alice
        .pay(&view(&mint), &request, Spacing::default())
        .unwrap();
    assert_eq!(payment.transfers.len(), 2);
    let paying = copy(&alice);

    // Alice submits the first transfer herself and keeps the payee's notes.
    let first = &payment.transfers[0].request;
    let answer = mint.transfer(first).unwrap();
    alice.settle(mint.keys(), first, &answer).unwrap();
    let owed: Vec<Note> = alice.payments().flat_map(|p| p.owed.clone()).collect();
    assert!(!owed.is_empty());

    // The copy that sent both learns the first was answered, though it paid
    // alice no change.
    let (merged, report) = merging(&paying, &alice);
    let kept: Vec<_> = merged.payments().collect();
    assert_eq!(kept.len(), 1, "one record for one payment");
    assert_eq!(kept[0].owed, owed);
    assert_eq!(kept[0].payment, payment);
    assert_eq!(report.ended, std::slice::from_ref(first));
    assert_eq!(merged.outgoing().count(), 1);
    assert_eq!(balance(&merged, &mint), balance(&alice, &mint));
    let (merged, report) = merging(&alice, &paying);
    assert_eq!(report, Merged::default());
    assert_eq!(balance(&merged, &mint), balance(&alice, &mint));

    // A transfer one copy cancelled leaves the payment in the merge, so the
    // payment is never handed out whole again.
    let second = &payment.transfers[1].request;
    let mut cancelled = copy(&alice);
    assert!(cancelled.cancel(second));
    let (merged, report) = merging(&alice, &cancelled);
    assert_eq!(report.ended, std::slice::from_ref(second));
    let kept: Vec<_> = merged.payments().collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].payment.transfers.len(), 1);
    assert!(request.check(&kept[0].payment).is_err());
}

#[test]
fn a_note_both_copies_spent_is_a_conflict_the_mint_settles() {
    let mut mint = issuer();
    let (mut alice, mut bob, mut carol) = (Holder::new(), Holder::new(), Holder::new());
    fund(&mut mint, &mut alice, &[8]);
    let mut device = copy(&alice);
    let to_bob = pay(&mint, &mut alice, &mut bob, 5);
    let to_carol = pay(&mint, &mut device, &mut carol, 3);

    let (mut merged, report) = merging(&alice, &device);
    assert_eq!(report.outgoing, std::slice::from_ref(&to_carol));
    assert_eq!(report.conflicts, [to_bob.clone(), to_carol.clone()]);
    assert_eq!(merged.notes().count(), 0);
    // Until the mint applies one, the note counts in both.
    assert_eq!(balance(&merged, &mint).outgoing, 16);

    // The mint applies one and refuses the other, which is then dropped.
    let answer = mint.transfer(&to_carol).unwrap();
    merged.settle(mint.keys(), &to_carol, &answer).unwrap();
    assert!(mint.transfer(&to_bob).is_err());
    assert!(merged.forget(&to_bob));
    let b = balance(&merged, &mint);
    assert_eq!((b.held(), b.outgoing, b.change), (5, 0, 0));
}

#[test]
fn holders_of_other_mints_merge_as_a_union() {
    let (mut one, mut two) = (issuer(), issuer());
    let (mut alice, mut other) = (Holder::new(), Holder::new());
    fund(&mut one, &mut alice, &[8]);
    fund(&mut two, &mut other, &[4]);
    other.request(&view(&two), 2).unwrap();

    let (merged, report) = merging(&alice, &other);
    assert_eq!(report.notes.len(), 1);
    assert_eq!(balance(&merged, &one), balance(&alice, &one));
    assert_eq!(counted(&merged, &two), balance(&other, &two));
    let mut mints = vec![*one.public_key(), *two.public_key()];
    mints.sort_unstable();
    assert_eq!(merged.mints(), mints);

    // Split by mint again, each part is what came in.
    let part = merged.only(&[*two.public_key()]);
    assert_eq!(part.mints(), [*two.public_key()]);
    assert_eq!(counted(&part, &two), balance(&other, &two));
    assert_eq!(balance(&part, &one), Balance::default());
    assert!(merged.only(&[]).mints().is_empty());

    // Joined, as the purses of two servers are for one backup, nothing is
    // unchecked.
    let mut joined = copy(&alice);
    joined.join(&other);
    assert_eq!(balance(&joined, &two), balance(&other, &two));
    assert_eq!(joined.unchecked().count(), 0);
}

#[test]
fn a_part_remembers_only_what_its_mints_ended() {
    let (mut one, mut two) = (issuer(), issuer());
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut one, &mut alice, &[4]);
    fund(&mut two, &mut alice, &[4]);
    let at_one = pay(&one, &mut alice, &mut bob, 4);
    let at_two = pay(&two, &mut alice, &mut bob, 4);
    let backup = copy(&alice);
    assert!(alice.cancel(&at_one));
    assert!(alice.cancel(&at_two));

    // The part of mint one ends only mint one's transfer: mint two's note,
    // which it does not hold, is not dropped as spent.
    let part = alice.only(&[*one.public_key()]);
    let (merged, report) = merging(&backup, &part);
    assert_eq!(report.ended, [at_one]);
    assert_eq!(merged.outgoing().collect::<Vec<_>>(), [&at_two]);
    assert_eq!(balance(&merged, &one).held(), 4);
    assert_eq!(balance(&merged, &two).outgoing, 4);
}

#[test]
fn a_mint_of_transfers_ended_alone_is_still_in_a_part() {
    let (mut one, mut two) = (issuer(), issuer());
    let (mut alice, mut bob) = (Holder::new(), Holder::new());
    fund(&mut one, &mut alice, &[4]);
    fund(&mut two, &mut alice, &[8]);
    let to_bob = pay(&one, &mut alice, &mut bob, 4);
    let older = copy(&alice);
    complete(&mut one, &mut alice, &to_bob);

    // Of mint one, Alice keeps only the record of the transfer she settled,
    // which stops an older copy from bringing it back: it is in the part for
    // mint one's server too.
    let mut mints = vec![*one.public_key(), *two.public_key()];
    mints.sort_unstable();
    assert_eq!(alice.mints(), mints);
    let part = alice.only(&[*one.public_key()]);
    assert_eq!(part.mints(), [*one.public_key()]);
    let (merged, report) = merging(&older, &part);
    assert_eq!(report.ended, [to_bob]);
    assert_eq!(balance(&merged, &one), Balance::default());
}

#[test]
fn the_transfers_ended_last_are_kept_up_to_the_bound_and_saved() {
    let mut mint = issuer();
    let mut alice = Holder::new();
    fund(&mut mint, &mut alice, &[1]);
    let first = pay(&mint, &mut alice, &mut Holder::new(), 1);
    let backup = copy(&alice);
    assert!(alice.cancel(&first));
    for _ in 0..ENDED_TRANSFERS_KEPT {
        let transfer = pay(&mint, &mut alice, &mut Holder::new(), 1);
        assert!(alice.cancel(&transfer));
    }
    let alice = copy(&alice);
    // The oldest is forgotten: a copy that still has it outgoing brings it
    // back, as from a store that kept no record.
    let transfer = first;
    let (merged, report) = merging(&alice, &backup);
    assert_eq!(report.outgoing, [transfer]);
    assert_eq!(merged.notes().count(), 0);
    let saved: serde_json::Value = serde_json::from_str(&alice.to_json()).unwrap();
    let ended = saved["ended"][mint.public_key().to_string()]
        .as_array()
        .unwrap();
    assert_eq!(ended.len(), ENDED_TRANSFERS_KEPT);
    // A store that ended nothing saves no record, as before there was one.
    assert!(!Holder::new().to_json().contains("ended"));
}

#[test]
fn the_notes_discarded_last_are_kept_up_to_the_bound_and_saved() {
    let mut mint = issuer();
    let mut alice = Holder::new();
    fund(&mut mint, &mut alice, &[1]);
    let backup = copy(&alice);
    let first: Vec<Note> = alice.notes().copied().collect();
    assert_eq!(alice.discard(&first), 1);
    for _ in 0..DISCARDED_NOTES_KEPT {
        fund(&mut mint, &mut alice, &[1]);
        let note: Vec<Note> = alice.notes().copied().collect();
        assert_eq!(alice.discard(&note), 1);
    }
    let alice = copy(&alice);
    // The oldest is forgotten: a copy that still holds it brings it back, as
    // from a store that kept no record.
    let (merged, report) = merging(&alice, &backup);
    assert_eq!(report.notes, first);
    assert_eq!(merged.unchecked().copied().collect::<Vec<_>>(), first);
    let saved: serde_json::Value = serde_json::from_str(&alice.to_json()).unwrap();
    let discarded = saved["discarded"][mint.public_key().to_string()]
        .as_array()
        .unwrap();
    assert_eq!(discarded.len(), DISCARDED_NOTES_KEPT);
    // A store that discarded nothing saves no record, as before there was
    // one.
    assert!(!Holder::new().to_json().contains("discarded"));
}
