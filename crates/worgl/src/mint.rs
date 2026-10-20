//! Mint requests signed by a mint authority.
//!
//! An issuer run as a service should not be able to create money on its own
//! say. Its policy therefore lists the public keys of the mint authorities it
//! accepts ([`Policy::mint_authorities`]), and each mint request carries a
//! signature from one of them over this message:
//!
//! ```text
//! b"worgl/mint/v1" ‖ issuer key (32) ‖ authority key (32)
//!   ‖ sequence (u64) ‖ amount (u64) ‖ holder key (32)
//! ```
//!
//! Each authority numbers its requests, and the issuer accepts only a
//! sequence number above the last one it accepted from that authority. A
//! copied request therefore cannot mint twice, at the cost of one counter
//! per authority. Resending the latest request, for instance after a lost
//! response, returns the same note. An authority should send one request at
//! a time and resend it until it is answered before using the next number.
//!
//! The issuer logs each authorization with the note it created, so
//! anyone holding the log can check that every mint was signed for
//! ([`MintAuthorization::verify`]).
//!
//! [`Policy::mint_authorities`]: crate::Policy::mint_authorities

use serde::{Deserialize, Serialize};

use crate::codec::Writer;
use crate::error::Error;
use crate::issuer::Issued;
use crate::keys::{MintKey, PublicKey, Signature};
use crate::transfer::Verifier;

const DOMAIN: &[u8] = b"worgl/mint/v1";

/// A request to mint one note, signed by a mint authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintRequest {
    /// The authority that signed the request.
    pub authority: PublicKey,
    /// Above every sequence number this authority used before.
    pub sequence: u64,
    /// Value of the note.
    pub amount: u64,
    /// The key the note is bound to.
    pub holder: PublicKey,
    /// The authority's signature over the request.
    pub signature: Signature,
}

impl MintRequest {
    /// Signs a request for `issuer` to mint `amount` to `holder`.
    pub fn sign(
        key: &MintKey,
        issuer: &PublicKey,
        sequence: u64,
        amount: u64,
        holder: PublicKey,
    ) -> Self {
        let authority = key.public_key();
        let signature = key.sign(&message(issuer, &authority, sequence, amount, &holder));
        Self {
            authority,
            sequence,
            amount,
            holder,
            signature,
        }
    }
}

fn message(
    issuer: &PublicKey,
    authority: &PublicKey,
    sequence: u64,
    amount: u64,
    holder: &PublicKey,
) -> Vec<u8> {
    let mut w = Writer::with_capacity(DOMAIN.len() + 112);
    w.bytes(DOMAIN)
        .bytes(issuer.as_bytes())
        .bytes(authority.as_bytes())
        .u64(sequence)
        .u64(amount)
        .bytes(holder.as_bytes());
    w.finish()
}

/// Who authorized a mint, as the log records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintAuthorization {
    /// The authority's public key.
    pub authority: PublicKey,
    /// The request's sequence number.
    pub sequence: u64,
    /// The authority's signature over the request.
    pub signature: Signature,
}

impl MintAuthorization {
    /// Whether this authorizes minting `issued` at the issuer `issuer`.
    pub fn verify(&self, issuer: &PublicKey, issued: &Issued) -> bool {
        let message = message(
            issuer,
            &self.authority,
            self.sequence,
            issued.amount,
            &issued.holder,
        );
        self.authority.verify(&message, &self.signature)
    }
}

/// A mint request whose signature and amount have been checked.
///
/// Only [`Verifier::verify_mint`] creates one. Whether its sequence number
/// is fresh is decided later, against the issuer's state, by
/// [`Issuer::plan_authorized_mint`](crate::Issuer::plan_authorized_mint).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedMint {
    pub(crate) issuer: PublicKey,
    pub(crate) authorization: MintAuthorization,
    pub(crate) amount: u64,
    pub(crate) holder: PublicKey,
}

impl VerifiedMint {
    /// The authorization to log.
    pub fn authorization(&self) -> &MintAuthorization {
        &self.authorization
    }

    /// Value of the note to mint.
    pub fn amount(&self) -> u64 {
        self.amount
    }
}

impl Verifier {
    /// Checks a mint request: the amount against the policy, that the policy
    /// lists the authority, and the authority's signature.
    pub fn verify_mint(&self, request: &MintRequest) -> Result<VerifiedMint, Error> {
        self.policy().check_mint(request.amount)?;
        if !self.policy().mint_authorities.contains(&request.authority) {
            return Err(Error::UnknownAuthority(request.authority));
        }
        let message = message(
            self.issuer(),
            &request.authority,
            request.sequence,
            request.amount,
            &request.holder,
        );
        if !request.authority.verify(&message, &request.signature) {
            return Err(Error::MintUnauthorized);
        }
        Ok(VerifiedMint {
            issuer: *self.issuer(),
            authorization: MintAuthorization {
                authority: request.authority,
                sequence: request.sequence,
                signature: request.signature,
            },
            amount: request.amount,
            holder: request.holder,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{HolderKey, IssuerKey};
    use crate::keyset::KeySet;
    use crate::ledger::Serial;
    use crate::policy::Policy;

    struct Fixture {
        issuer: PublicKey,
        mint: MintKey,
        verifier: Verifier,
    }

    impl Fixture {
        fn new() -> Self {
            let issuer = IssuerKey::generate().public_key();
            let mint = MintKey::generate();
            let policy = Policy {
                mint_authorities: vec![mint.public_key()],
                ..Policy::cash_like()
            };
            let keys = KeySet::new(issuer, 0, Vec::new()).unwrap();
            Self {
                issuer,
                verifier: Verifier::new(keys.clone(), keys, policy),
                mint,
            }
        }

        fn request(&self, sequence: u64, amount: u64) -> MintRequest {
            let holder = HolderKey::generate().public_key();
            MintRequest::sign(&self.mint, &self.issuer, sequence, amount, holder)
        }
    }

    #[test]
    fn a_listed_authority_can_mint_and_the_log_can_prove_it() {
        let f = Fixture::new();
        let request = f.request(1, 8);
        let verified = f.verifier.verify_mint(&request).unwrap();
        assert_eq!(verified.amount(), 8);
        let issued = Issued {
            serial: Serial::new(0, 0),
            amount: 8,
            holder: request.holder,
        };
        let authorization = verified.authorization();
        assert!(authorization.verify(&f.issuer, &issued));
        let more = Issued {
            amount: 16,
            ..issued
        };
        assert!(!authorization.verify(&f.issuer, &more));
        let elsewhere = IssuerKey::generate().public_key();
        assert!(!authorization.verify(&elsewhere, &issued));
    }

    #[test]
    fn unlisted_keys_bad_signatures_and_bad_amounts_are_refused() {
        let f = Fixture::new();
        let stranger = MintKey::generate();
        let holder = HolderKey::generate().public_key();
        let request = MintRequest::sign(&stranger, &f.issuer, 1, 8, holder);
        assert_eq!(
            f.verifier.verify_mint(&request),
            Err(Error::UnknownAuthority(stranger.public_key()))
        );

        let mut request = f.request(1, 8);
        request.amount = 16;
        assert_eq!(
            f.verifier.verify_mint(&request),
            Err(Error::MintUnauthorized)
        );
        let mut request = f.request(1, 8);
        request.sequence = 2;
        assert_eq!(
            f.verifier.verify_mint(&request),
            Err(Error::MintUnauthorized)
        );

        // Signed for another issuer.
        let other = IssuerKey::generate().public_key();
        let request = MintRequest::sign(&f.mint, &other, 1, 8, holder);
        assert_eq!(
            f.verifier.verify_mint(&request),
            Err(Error::MintUnauthorized)
        );

        // 3 is not a power of two.
        assert!(matches!(
            f.verifier.verify_mint(&f.request(1, 3)),
            Err(Error::Policy(_))
        ));
    }

    #[test]
    fn requests_round_trip_as_json() {
        let f = Fixture::new();
        let request = f.request(7, 8);
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<MintRequest>(&json).unwrap(), request);
    }
}
