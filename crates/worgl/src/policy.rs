//! The issuer's rules, kept apart from the ledger so they can change freely.
//!
//! The ledger only knows bits and layers. Everything here is a choice an
//! operator can make differently without touching stored state: which amounts
//! may exist, how many notes a transfer may consume and create, where new
//! money lands, who may ask for it to be minted and how many recent transfers
//! can be safely retried.
//!
//! The default, [`Policy::cash_like`], is chosen for privacy. A transfer that
//! consumes several notes tells the issuer those notes had one owner, the
//! same heuristic that links addresses on public blockchains. So the default
//! allows exactly one input per transfer. Fixed denominations keep notes
//! interchangeable, and a payment of an odd amount is several one-note
//! transfers, with any change handed back by the payee the same way. A
//! transfer may still split one note into several; that reveals nothing the
//! issuer did not already know, since all the outputs descend from one note.
//! [`Policy::flexible`] lifts both limits for systems that do not need this.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::keys::PublicKey;
use crate::ledger::LedgerConfig;

/// Which amounts a note may carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Denominations {
    /// Any positive amount.
    Any,
    /// 1, 2, 4, 8 and so on up to 2⁶³. Every amount splits into at most one
    /// note of each size.
    PowersOfTwo,
    /// An explicit set, strictly ascending and without zero. Build it with
    /// [`Denominations::list`].
    List(Vec<u64>),
}

impl Denominations {
    /// A validated explicit set; the values may be given in any order.
    pub fn list(values: impl IntoIterator<Item = u64>) -> Result<Self, PolicyError> {
        let mut values: Vec<u64> = values.into_iter().collect();
        values.sort_unstable();
        values.dedup();
        let list = Self::List(values);
        list.validate()?;
        Ok(list)
    }

    /// Whether a note may carry `amount`.
    pub fn contains(&self, amount: u64) -> bool {
        match self {
            Self::Any => amount > 0,
            Self::PowersOfTwo => amount.is_power_of_two(),
            Self::List(values) => values.binary_search(&amount).is_ok(),
        }
    }

    /// Splits `amount` into allowed denominations as `(denomination, count)`
    /// pairs, largest first, or `None` if the greedy split does not come out
    /// exact.
    ///
    /// Greedy splitting is exact and minimal for powers of two and for the
    /// usual 1-2-5 series. For unusual explicit sets it can miss a split that
    /// exists.
    pub fn decompose(&self, amount: u64) -> Option<Vec<(u64, u64)>> {
        match self {
            _ if amount == 0 => None,
            Self::Any => Some(vec![(amount, 1)]),
            Self::PowersOfTwo => Some(
                (0..64)
                    .rev()
                    .map(|bit| 1u64 << bit)
                    .filter(|value| amount & value != 0)
                    .map(|value| (value, 1))
                    .collect(),
            ),
            Self::List(values) => {
                let mut rest = amount;
                let mut parts = Vec::new();
                for &value in values.iter().rev() {
                    let count = rest / value;
                    if count > 0 {
                        rest -= count * value;
                        parts.push((value, count));
                    }
                }
                (rest == 0).then_some(parts)
            }
        }
    }

    fn validate(&self) -> Result<(), PolicyError> {
        if let Self::List(values) = self {
            let ascending = values.windows(2).all(|pair| pair[0] < pair[1]);
            if values.is_empty() || values[0] == 0 || !ascending {
                return Err(PolicyError::Denominations);
            }
        }
        Ok(())
    }
}

/// The issuer's rules for minting and transferring. See the [module documentation](self).
///
/// When deserialized, missing fields take their [`Policy::cash_like`] values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    /// Position of the layer new money is minted into. `None` means the
    /// second to last layer.
    ///
    /// A note at position `k` of `n` layers can make `n − 1 − k` more hops
    /// before the next rotation and, untouched, dies in the `(k + 1)`-th
    /// rotation from now.
    /// The two always add up to `n`, so this trades spending room for
    /// lifetime. The second to last layer allows one hop right away and the
    /// longest life that does.
    pub mint_layer: Option<u32>,
    /// Most notes one transfer may consume. 1 forbids merging.
    pub max_inputs: u8,
    /// Most notes one transfer may create.
    pub max_outputs: u8,
    /// Amounts a minted or transferred note may carry.
    pub denominations: Denominations,
    /// How many recent transfers the issuer remembers, so that a client whose
    /// response was lost can resend the identical request and get the same
    /// notes back. 0 turns this off. Each entry costs about
    /// `40 + 52 × outputs` bytes.
    pub receipt_capacity: u32,
    /// Keys whose signed [`MintRequest`](crate::MintRequest)s the issuer
    /// accepts. Empty, the default, accepts none. Removing a key stops it
    /// minting; the issuer keeps its sequence number, so adding it back
    /// cannot replay its old requests.
    pub mint_authorities: Vec<PublicKey>,
}

impl Policy {
    /// One input per transfer and powers-of-two denominations. The default.
    pub fn cash_like() -> Self {
        Self {
            mint_layer: None,
            max_inputs: 1,
            max_outputs: 16,
            denominations: Denominations::PowersOfTwo,
            receipt_capacity: 65_536,
            mint_authorities: Vec::new(),
        }
    }

    /// Up to 16 inputs and 16 outputs of any amount, so transfers can merge
    /// notes and pay change in one step. The issuer learns that merged notes
    /// shared an owner.
    pub fn flexible() -> Self {
        Self {
            max_inputs: 16,
            denominations: Denominations::Any,
            ..Self::cash_like()
        }
    }

    /// Checks the policy is coherent and fits a ledger of this shape.
    pub fn validate(&self, ledger: LedgerConfig) -> Result<(), PolicyError> {
        if let Some(mint_layer) = self.mint_layer
            && mint_layer >= ledger.layers
        {
            return Err(PolicyError::MintLayer {
                mint_layer,
                layers: ledger.layers,
            });
        }
        if self.max_inputs == 0 || self.max_outputs == 0 {
            return Err(PolicyError::Limits);
        }
        self.denominations.validate()
    }

    /// The position new money is minted into, for a ledger of this shape.
    pub fn mint_position(&self, ledger: LedgerConfig) -> u32 {
        self.mint_layer.unwrap_or(ledger.layers - 2)
    }

    /// Checks a mint of `amount`.
    pub fn check_mint(&self, amount: u64) -> Result<(), PolicyError> {
        self.check_amount(amount)
    }

    /// Checks a transfer's shape: counts, output denominations and that
    /// outputs add up exactly to inputs. Input amounts are not checked against
    /// the denominations, so notes issued under an earlier policy still spend.
    pub fn check_transfer(&self, inputs: &[u64], outputs: &[u64]) -> Result<(), PolicyError> {
        if inputs.is_empty() || inputs.len() > usize::from(self.max_inputs) {
            return Err(PolicyError::Inputs {
                count: inputs.len(),
                max: self.max_inputs,
            });
        }
        if outputs.is_empty() || outputs.len() > usize::from(self.max_outputs) {
            return Err(PolicyError::Outputs {
                count: outputs.len(),
                max: self.max_outputs,
            });
        }
        for &amount in outputs {
            self.check_amount(amount)?;
        }
        let total = |amounts: &[u64]| amounts.iter().map(|&a| u128::from(a)).sum::<u128>();
        let (inputs, outputs) = (total(inputs), total(outputs));
        if inputs != outputs {
            return Err(PolicyError::Unbalanced { inputs, outputs });
        }
        Ok(())
    }

    /// Checks a burn of `inputs` notes: at least one, and no more than a
    /// transfer may spend, so a burn reveals no more co-ownership than a
    /// transfer would.
    pub fn check_burn(&self, inputs: usize) -> Result<(), PolicyError> {
        if inputs == 0 || inputs > usize::from(self.max_inputs) {
            return Err(PolicyError::Inputs {
                count: inputs,
                max: self.max_inputs,
            });
        }
        Ok(())
    }

    fn check_amount(&self, amount: u64) -> Result<(), PolicyError> {
        if amount == 0 {
            return Err(PolicyError::ZeroAmount);
        }
        if !self.denominations.contains(amount) {
            return Err(PolicyError::Denomination(amount));
        }
        Ok(())
    }
}

impl Default for Policy {
    fn default() -> Self {
        Self::cash_like()
    }
}

/// A policy is incoherent, or a request breaks it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PolicyError {
    /// The mint layer does not exist in the ledger.
    #[error("mint layer {mint_layer} does not exist in a ledger of {layers} layers")]
    MintLayer {
        /// The configured mint layer.
        mint_layer: u32,
        /// Layers in the ledger.
        layers: u32,
    },
    /// `max_inputs` or `max_outputs` is zero.
    #[error("a transfer must allow at least one input and one output")]
    Limits,
    /// An explicit denomination list is empty, contains zero or is not strictly ascending.
    #[error("denominations must be a non-empty, strictly ascending list without zero")]
    Denominations,
    /// The transfer or burn has no inputs or too many.
    #[error("a transfer or burn takes 1 to {max} input notes, not {count}")]
    Inputs {
        /// Inputs given.
        count: usize,
        /// Most allowed.
        max: u8,
    },
    /// The transfer has no outputs or too many.
    #[error("a transfer makes 1 to {max} notes, not {count}")]
    Outputs {
        /// Outputs given.
        count: usize,
        /// Most allowed.
        max: u8,
    },
    /// An amount is zero.
    #[error("amounts must be positive")]
    ZeroAmount,
    /// An amount is not an allowed denomination.
    #[error("{0} is not an allowed denomination")]
    Denomination(u64),
    /// Outputs do not add up to inputs.
    #[error("outputs add up to {outputs}, inputs to {inputs}")]
    Unbalanced {
        /// Sum of input amounts.
        inputs: u128,
        /// Sum of output amounts.
        outputs: u128,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEDGER: LedgerConfig = LedgerConfig {
        layers: 4,
        layer_size: 64,
    };

    #[test]
    fn cash_like_allows_one_input_and_splits() {
        let p = Policy::cash_like();
        assert_eq!(p.check_transfer(&[8], &[8]), Ok(()));
        assert_eq!(p.check_transfer(&[8], &[4, 2, 2]), Ok(()));
        assert_eq!(
            p.check_transfer(&[4, 4], &[8]),
            Err(PolicyError::Inputs { count: 2, max: 1 })
        );
        assert_eq!(
            p.check_transfer(&[8], &[5, 3]),
            Err(PolicyError::Denomination(5))
        );
        assert_eq!(
            p.check_transfer(&[8], &[4]),
            Err(PolicyError::Unbalanced {
                inputs: 8,
                outputs: 4
            })
        );
        assert_eq!(p.check_mint(0), Err(PolicyError::ZeroAmount));
        assert_eq!(p.mint_position(LEDGER), 2);
    }

    #[test]
    fn flexible_allows_merging_and_any_amount() {
        let p = Policy::flexible();
        assert_eq!(p.check_transfer(&[7, 3], &[9, 1]), Ok(()));
        assert!(p.check_transfer(&[1; 17], &[17]).is_err());
    }

    #[test]
    fn balances_are_summed_without_overflow() {
        let p = Policy::flexible();
        assert_eq!(
            p.check_transfer(&[u64::MAX, u64::MAX], &[u64::MAX, u64::MAX]),
            Ok(())
        );
    }

    #[test]
    fn validates_against_the_ledger() {
        let mut p = Policy::cash_like();
        p.mint_layer = Some(4);
        assert!(matches!(
            p.validate(LEDGER),
            Err(PolicyError::MintLayer { .. })
        ));
        p.mint_layer = Some(3);
        assert_eq!(p.validate(LEDGER), Ok(()));
        p.max_inputs = 0;
        assert_eq!(p.validate(LEDGER), Err(PolicyError::Limits));
        let bad = Policy {
            denominations: Denominations::List(vec![5, 1]),
            ..Policy::cash_like()
        };
        assert_eq!(bad.validate(LEDGER), Err(PolicyError::Denominations));
    }

    #[test]
    fn decomposes_amounts() {
        assert_eq!(
            Denominations::PowersOfTwo.decompose(13),
            Some(vec![(8, 1), (4, 1), (1, 1)])
        );
        let decimal = Denominations::list([1, 2, 5, 10, 20, 50]).unwrap();
        assert_eq!(
            decimal.decompose(197),
            Some(vec![(50, 3), (20, 2), (5, 1), (2, 1)])
        );
        assert_eq!(Denominations::list([5, 10]).unwrap().decompose(7), None);
        assert_eq!(Denominations::Any.decompose(0), None);
        assert!(Denominations::list([]).is_err());
        assert!(Denominations::list([0, 1]).is_err());
    }

    #[test]
    fn missing_fields_default_to_cash_like() {
        let policy: Policy = serde_json::from_str(r#"{"max_inputs": 4}"#).unwrap();
        assert_eq!(policy.max_inputs, 4);
        assert_eq!(policy.denominations, Denominations::PowersOfTwo);
    }

    #[test]
    fn serializes_readably() {
        let json = serde_json::to_value(Policy::cash_like()).unwrap();
        assert_eq!(json["denominations"], "powers_of_two");
        let list = serde_json::to_value(Denominations::list([1, 5]).unwrap()).unwrap();
        assert_eq!(list, serde_json::json!({"list": [1, 5]}));
    }
}
