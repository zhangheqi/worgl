//! Payments between holders, carried out by a real issuer in memory.

use std::collections::HashSet;

use proptest::prelude::*;
use worgl::{Issuer, IssuerKey, LedgerConfig, Note, Policy};
use worgl_sdk::{
    CheckError, Holder, MintInfo, MintView, NoteState, PayError, Payment, Schedule, Spacing,
};

fn issuer(layers: u32) -> Issuer {
    let ledger = LedgerConfig {
        layers,
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

fn total(holder: &Holder, view: &MintView) -> u128 {
    let b = holder.balance(view);
    b.spendable + b.frozen + b.unknown
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

    mint.rotate();
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
    mint.rotate();
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

    mint.rotate();
    let v = view(&mint);
    assert_eq!(alice.prune(&v), 6 - refreshed);
    assert_eq!(total(&alice, &v), refreshed);
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
    alice.cancel(&payment.transfers[0].request);
    let balance = alice.balance(&view(&mint));
    assert_eq!(
        (balance.spendable, balance.outgoing, balance.expected),
        (8, 0, 0)
    );
}

#[test]
fn a_view_follows_the_schedule() {
    let mint = issuer(4);
    let mut v = view(&mint);
    v.schedule = Some(Schedule {
        period_secs: 100,
        next_at: 1_000,
    });
    assert_eq!(v.at(999), v);
    assert_eq!(v.until_rotation(990), Some(10));
    assert_eq!(v.at(1_000).oldest, v.oldest + 1);
    let later = v.at(1_250);
    assert_eq!(later.oldest, v.oldest + 3);
    assert_eq!(later.schedule.unwrap().next_at, 1_300);
    assert_eq!(v.until_rotation(1_250), Some(50));

    v.paused = true;
    assert_eq!(v.at(5_000), v);
    assert_eq!(v.until_rotation(5_000), None);
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
                prop_assert_eq!(total(&alice, &view(&mint)), held - u128::from(amount));
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
