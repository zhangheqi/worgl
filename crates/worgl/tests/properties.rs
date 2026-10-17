//! Property tests: the ledger against a naive model, and the issuer's
//! invariants over random histories of mints, transfers and rotations.

use std::collections::{BTreeSet, VecDeque};

use proptest::prelude::*;
use worgl::ledger::Status;
use worgl::{
    Denominations, Event, HolderKey, Issuer, IssuerKey, Ledger, LedgerConfig, Note, Output, Plan,
    Policy, Serial, TransferRequest,
};

#[derive(Debug, Clone)]
enum LedgerOp {
    Allocate(u32),
    Invalidate(usize),
    Rotate,
}

fn ledger_op() -> impl Strategy<Value = LedgerOp> {
    prop_oneof![
        6 => any::<u32>().prop_map(LedgerOp::Allocate),
        4 => any::<usize>().prop_map(LedgerOp::Invalidate),
        1 => Just(LedgerOp::Rotate),
    ]
}

/// The obvious, slow implementation: a queue of layers with sets of live indexes.
struct ModelLayer {
    generation: u64,
    next: u32,
    live: BTreeSet<u32>,
}

struct Model {
    layer_size: u32,
    layers: VecDeque<ModelLayer>,
}

impl Model {
    fn new(config: LedgerConfig) -> Self {
        Self {
            layer_size: config.layer_size,
            layers: (0..u64::from(config.layers))
                .map(|generation| ModelLayer {
                    generation,
                    next: 0,
                    live: BTreeSet::new(),
                })
                .collect(),
        }
    }

    fn status(&self, serial: Serial) -> Status {
        if serial.generation < self.layers[0].generation {
            return Status::Expired;
        }
        let found = self
            .layers
            .iter()
            .enumerate()
            .find(|(_, l)| l.generation == serial.generation);
        match found {
            None => Status::Unissued,
            Some((_, l)) if serial.index >= l.next => Status::Unissued,
            Some((p, l)) if l.live.contains(&serial.index) => Status::Live { position: p as u32 },
            Some(_) => Status::Spent,
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn ledger_matches_the_model(
        layers in 2u32..6,
        words in 1u32..3,
        ops in prop::collection::vec(ledger_op(), 1..300),
    ) {
        let config = LedgerConfig { layers, layer_size: 64 * words };
        let mut ledger = Ledger::new(config).unwrap();
        let mut model = Model::new(config);
        let mut issued: Vec<Serial> = Vec::new();

        for op in ops {
            match op {
                LedgerOp::Allocate(p) => {
                    let position = p % layers;
                    let layer = &mut model.layers[position as usize];
                    let result = ledger.allocate(position);
                    if layer.next == model.layer_size {
                        prop_assert!(result.is_err());
                    } else {
                        let serial = result.unwrap();
                        prop_assert_eq!(serial, Serial::new(layer.generation, layer.next));
                        layer.live.insert(layer.next);
                        layer.next += 1;
                        issued.push(serial);
                    }
                }
                LedgerOp::Invalidate(k) if !issued.is_empty() => {
                    let serial = issued[k % issued.len()];
                    let live = matches!(model.status(serial), Status::Live { .. });
                    prop_assert_eq!(ledger.invalidate(serial).is_ok(), live);
                    if live {
                        let layer = model
                            .layers
                            .iter_mut()
                            .find(|l| l.generation == serial.generation)
                            .unwrap();
                        layer.live.remove(&serial.index);
                    }
                }
                LedgerOp::Invalidate(_) => {}
                LedgerOp::Rotate => {
                    let dropped = model.layers.pop_front().unwrap();
                    let generation = model.layers.back().unwrap().generation + 1;
                    model.layers.push_back(ModelLayer { generation, next: 0, live: BTreeSet::new() });
                    let rotation = ledger.rotate();
                    prop_assert_eq!(rotation.dropped, dropped.generation);
                    prop_assert_eq!(rotation.destroyed as usize, dropped.live.len());
                    prop_assert_eq!(rotation.created, generation);
                }
            }
            for &serial in &issued {
                prop_assert_eq!(ledger.status(serial), model.status(serial));
            }
            for (info, layer) in ledger.layers().zip(&model.layers) {
                prop_assert_eq!(info.generation, layer.generation);
                prop_assert_eq!(info.issued, layer.next);
                prop_assert_eq!(info.live as usize, layer.live.len());
            }
        }
        prop_assert_eq!(Ledger::from_bytes(&ledger.to_bytes()).unwrap(), ledger);
    }
}

#[derive(Debug, Clone)]
enum IssuerOp {
    Mint(u8),
    /// Spend the wallet's k-th note, split into up to `parts` notes.
    Transfer {
        k: usize,
        parts: u8,
    },
    /// Merge the wallet's k-th and j-th notes (flexible policy only).
    Merge {
        k: usize,
        j: usize,
    },
    Rotate,
}

fn issuer_op() -> impl Strategy<Value = IssuerOp> {
    prop_oneof![
        3 => any::<u8>().prop_map(IssuerOp::Mint),
        6 => (any::<usize>(), 1u8..4).prop_map(|(k, parts)| IssuerOp::Transfer { k, parts }),
        2 => (any::<usize>(), any::<usize>()).prop_map(|(k, j)| IssuerOp::Merge { k, j }),
        1 => Just(IssuerOp::Rotate),
    ]
}

/// Deterministic holder keys, so failures shrink and reproduce.
struct Keys(u64);

impl Keys {
    fn next(&mut self) -> HolderKey {
        self.0 += 1;
        let mut secret = [0u8; 32];
        secret[..8].copy_from_slice(&self.0.to_be_bytes());
        HolderKey::from_secret_bytes(&secret)
    }
}

fn split(amount: u64, parts: u8, policy: &Policy) -> Vec<u64> {
    match policy.denominations {
        // Halve a power of two `parts − 1` times: 8 → 4, 2, 2.
        Denominations::PowersOfTwo => {
            let mut out = vec![amount];
            for _ in 1..parts {
                let last = *out.last().unwrap();
                if last < 2 {
                    break;
                }
                out.pop();
                out.extend([last / 2, last / 2]);
            }
            out
        }
        _ => {
            let parts = u64::from(parts).min(amount);
            let mut out = vec![amount / parts; parts as usize];
            out[0] += amount % parts;
            out
        }
    }
}

fn run_history(policy: Policy, ops: Vec<IssuerOp>) -> Result<(), TestCaseError> {
    let config = LedgerConfig {
        layers: 4,
        layer_size: 64,
    };
    let key = IssuerKey::from_secret_bytes(&[9; 32]);
    let mut issuer = Issuer::new(key.clone(), config, policy.clone()).unwrap();
    let genesis = issuer.snapshot();
    let mut keys = Keys(0);
    let mut wallet: Vec<(Note, HolderKey)> = Vec::new();
    let mut spent: Vec<(Note, HolderKey)> = Vec::new();
    let mut events: Vec<Event> = Vec::new();

    for op in ops {
        let mut commit = |issuer: &mut Issuer, event: Event| {
            issuer.apply(&event).unwrap();
            let notes = issuer.notes(&event);
            events.push(event);
            notes
        };
        match op {
            IssuerOp::Mint(exp) => {
                let amount = match policy.denominations {
                    Denominations::PowersOfTwo => 1u64 << (exp % 12),
                    _ => u64::from(exp) + 1,
                };
                let holder = keys.next();
                if let Ok(event) = issuer.plan_mint(amount, holder.public_key()) {
                    let note = commit(&mut issuer, event).remove(0);
                    wallet.push((note, holder));
                }
            }
            IssuerOp::Transfer { k, parts } if !wallet.is_empty() => {
                let (note, holder) = wallet.remove(k % wallet.len());
                let outputs: Vec<(Output, HolderKey)> = split(note.amount(), parts, &policy)
                    .into_iter()
                    .map(|amount| {
                        let key = keys.next();
                        (
                            Output {
                                amount,
                                holder: key.public_key(),
                            },
                            key,
                        )
                    })
                    .collect();
                let request = TransferRequest::sign(
                    &[(note, &holder)],
                    outputs.iter().map(|(o, _)| *o).collect(),
                )
                .unwrap();
                let verified = issuer.verifier().verify(&request).unwrap();
                match issuer.plan_transfer(&verified) {
                    Ok(Plan::Apply(event)) => {
                        let notes = commit(&mut issuer, event);
                        prop_assert_eq!(notes.len(), outputs.len());
                        wallet.extend(notes.into_iter().zip(outputs.into_iter().map(|(_, k)| k)));
                        spent.push((note, holder));
                    }
                    Ok(Plan::Replay(_)) => prop_assert!(false, "fresh outputs never replay"),
                    // Frozen in the last layer, or the next layer is full.
                    Err(_) => wallet.push((note, holder)),
                }
            }
            IssuerOp::Merge { k, j }
                if wallet.len() >= 2 && k % wallet.len() != j % wallet.len() =>
            {
                let (a, ka) = wallet[k % wallet.len()].clone();
                let (b, kb) = wallet[j % wallet.len()].clone();
                let out = keys.next();
                let total = a.amount() + b.amount();
                let request = TransferRequest::sign(
                    &[(a, &ka), (b, &kb)],
                    vec![Output {
                        amount: total,
                        holder: out.public_key(),
                    }],
                )
                .unwrap();
                let verified = issuer.verifier().verify(&request);
                if policy.max_inputs < 2 {
                    prop_assert!(verified.is_err(), "the policy forbids merging");
                    continue;
                }
                if let Ok(Plan::Apply(event)) = issuer.plan_transfer(&verified.unwrap()) {
                    let note = commit(&mut issuer, event).remove(0);
                    wallet.retain(|(n, _)| n != &a && n != &b);
                    wallet.push((note, out));
                    spent.push((a, ka));
                    spent.push((b, kb));
                }
            }
            IssuerOp::Transfer { .. } | IssuerOp::Merge { .. } => {}
            IssuerOp::Rotate => {
                let event = issuer.plan_rotate();
                commit(&mut issuer, event);
                let oldest = issuer.ledger().oldest_generation();
                wallet.retain(|(note, _)| note.serial().generation >= oldest);
            }
        }

        // Value is conserved: what is live is what was minted minus what expired.
        let supply = issuer.supply();
        prop_assert_eq!(supply.outstanding, supply.minted - supply.destroyed);
        let held: u128 = wallet.iter().map(|(n, _)| u128::from(n.amount())).sum();
        prop_assert_eq!(held, supply.outstanding);
        for (note, _) in &wallet {
            prop_assert!(issuer.ledger().is_live(note.serial()));
        }
    }

    // No spent note can be spent again.
    for (note, holder) in spent {
        let output = Output {
            amount: note.amount(),
            holder: keys.next().public_key(),
        };
        let request = TransferRequest::single(note, &holder, output).unwrap();
        let before = issuer.sequence();
        if let Ok(verified) = issuer.verifier().verify(&request) {
            prop_assert!(issuer.plan_transfer(&verified).is_err());
        }
        prop_assert_eq!(issuer.sequence(), before);
    }

    // Replaying the events onto the starting snapshot rebuilds the same state.
    let mut replayed = Issuer::restore(key, &genesis, policy).unwrap();
    for event in &events {
        replayed.apply(event).unwrap();
        prop_assert_eq!(Event::from_bytes(&event.to_bytes()).unwrap(), event.clone());
    }
    prop_assert_eq!(replayed.snapshot(), issuer.snapshot());
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn cash_like_histories_keep_the_invariants(ops in prop::collection::vec(issuer_op(), 1..120)) {
        run_history(Policy::cash_like(), ops)?;
    }

    #[test]
    fn flexible_histories_keep_the_invariants(ops in prop::collection::vec(issuer_op(), 1..120)) {
        run_history(Policy::flexible(), ops)?;
    }
}

proptest! {
    #[test]
    fn altering_any_byte_of_a_note_is_detected(offset in 0usize..Note::LEN, mask in 1u8..=255) {
        let key = IssuerKey::from_secret_bytes(&[3; 32]);
        let mut issuer = Issuer::new(
            key,
            LedgerConfig { layers: 2, layer_size: 64 },
            Policy::flexible(),
        )
        .unwrap();
        let note = issuer.mint(1000, HolderKey::from_secret_bytes(&[4; 32]).public_key()).unwrap();
        let mut bytes = note.to_bytes();
        bytes[offset] ^= mask;
        if let Ok(altered) = Note::from_bytes(&bytes) {
            prop_assert!(issuer.status(&altered).is_err());
        }
    }

    #[test]
    fn decomposition_is_exact(amount in 1u64..) {
        let decimal = Denominations::list([1, 2, 5, 10, 20, 50, 100]).unwrap();
        for denominations in [Denominations::PowersOfTwo, decimal] {
            let parts = denominations.decompose(amount).unwrap();
            let total: u128 = parts.iter().map(|&(v, n)| u128::from(v) * u128::from(n)).sum();
            prop_assert_eq!(total, u128::from(amount));
            prop_assert!(parts.iter().all(|&(v, _)| denominations.contains(v)));
        }
    }
}
