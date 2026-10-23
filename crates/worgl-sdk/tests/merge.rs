//! Merging two copies of one purse, against a real issuer in memory.

use worgl::{Issuer, IssuerKey, LedgerConfig, Note, NoteStatus, Policy};
use worgl_sdk::{Balance, Holder, Merged, MintInfo, MintView, Spacing};

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

/// `holder` with `other` merged in, and what the merge said.
fn merging(holder: &Holder, other: &Holder) -> (Holder, Merged) {
    let mut holder = copy(holder);
    let report = holder.merge(other);
    (holder, report)
}

/// Pays `amount` from `payer` to a fresh request of `payee`, and returns the
/// one transfer.
fn pay(
    issuer: &Issuer,
    payer: &mut Holder,
    payee: &mut Holder,
    amount: u64,
) -> worgl::TransferRequest {
    let request = payee.request(&view(issuer), amount).unwrap();
    let payment = payer
        .pay(&view(issuer), &request, Spacing::default())
        .unwrap();
    assert_eq!(payment.transfers.len(), 1);
    payment.transfers[0].request.clone()
}

/// A holder's balance at `issuer`, whose sums are checked against its parts.
fn balance(holder: &Holder, issuer: &Issuer) -> Balance {
    let b = holder.balance(&view(issuer));
    assert_eq!(b.expected, b.requested + b.change + b.refreshing, "{b:?}");
    b
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

    // Restored into an empty purse, the backup is all new.
    let (fresh, report) = merging(&Holder::new(), &backup);
    assert_eq!(report.notes, backup.notes().copied().collect::<Vec<_>>());
    assert_eq!(balance(&fresh, &mint), balance(&backup, &mint));

    let (merged, report) = merging(&alice, &backup);
    assert!(report.notes.is_empty(), "{report:?}");
    assert_eq!(merged.notes().count(), 2);
    assert_eq!(balance(&merged, &mint).total(), 12);
    // The other way round, the backup takes in what came after it.
    let (merged, report) = merging(&backup, &alice);
    assert_eq!(
        report.notes.iter().map(Note::amount).collect::<Vec<_>>(),
        [4]
    );
    assert_eq!(balance(&merged, &mint), balance(&alice, &mint));
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
    let (merged, report) = merging(&backup, &alice);
    assert_eq!(report.outgoing, [transfer]);
    assert!(report.notes.is_empty());
    assert_eq!(balance(&merged, &mint), paying);
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
    // change, so its key is no longer expected and nothing counts twice.
    let (merged, report) = merging(&sending, &alice);
    assert_eq!(report.settled, std::slice::from_ref(&transfer));
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

    // Spent again since, in a transfer not yet answered, the change still
    // shows the first was answered.
    mint.rotate().unwrap();
    let again = pay(&mint, &mut alice, &mut bob, 2);
    let (merged, report) = merging(&sending, &alice);
    assert_eq!(report.settled, [transfer]);
    assert_eq!(report.outgoing, [again]);
    assert_eq!(balance(&merged, &mint), balance(&alice, &mint));
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
}

#[test]
fn requests_from_either_copy_are_expected_until_paid() {
    let mut mint = issuer();
    let (mut alice, mut carol) = (Holder::new(), Holder::new());
    fund(&mut mint, &mut carol, &[16]);
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
    assert_eq!((b.held(), b.requested), (4, 2));

    // A request one copy withdrew is expected again: a payer may still pay.
    let open = device.request(&view(&mint), 1).unwrap();
    let mut withdrawn = copy(&device);
    withdrawn.withdraw(&open);
    assert_eq!(balance(&withdrawn, &mint).requested, 2);
    let (merged, _) = merging(&withdrawn, &device);
    assert_eq!(balance(&merged, &mint).requested, 3);
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

    // The copy that sent both learns the first was answered from the note
    // kept for the payee, though it paid alice no change.
    let (merged, report) = merging(&paying, &alice);
    let kept: Vec<_> = merged.payments().collect();
    assert_eq!(kept.len(), 1, "one record for one payment");
    assert_eq!(kept[0].owed, owed);
    assert_eq!(kept[0].payment, payment);
    assert_eq!(report.settled, std::slice::from_ref(first));
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
fn notes_a_backup_holds_that_were_spent_since_are_found_by_the_mint() {
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

    // Nothing in either copy tells that the backup's 8 is spent.
    let (mut merged, report) = merging(&alice, &backup);
    assert_eq!(
        report.notes.iter().map(Note::amount).collect::<Vec<_>>(),
        [8]
    );
    assert_eq!(balance(&merged, &mint).total(), 13);
    // The mint does: drop what it reports spent or expired, keep the rest.
    let gone: Vec<Note> = report
        .notes
        .iter()
        .filter(|note| {
            matches!(
                mint.status(note).unwrap(),
                NoteStatus::Spent | NoteStatus::Expired
            )
        })
        .copied()
        .collect();
    assert_eq!(merged.discard(&gone), 8);
    assert_eq!(balance(&merged, &mint), balance(&alice, &mint));
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
    assert_eq!(balance(&merged, &two), balance(&other, &two));
    let mut mints = vec![*one.public_key(), *two.public_key()];
    mints.sort_unstable();
    assert_eq!(merged.mints(), mints);

    // Split by mint again, each part is what came in.
    let part = merged.only(&[*two.public_key()]);
    assert_eq!(part.mints(), [*two.public_key()]);
    assert_eq!(balance(&part, &two), balance(&other, &two));
    assert_eq!(balance(&part, &one), Balance::default());
    assert!(merged.only(&[]).mints().is_empty());
}
