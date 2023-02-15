use std::ops::Mul;

use anyhow::Result;
use ark_ec::pairing::Pairing;
use ark_ec::CurveGroup;
use ark_ff::{Field, One, Zero};
use ferveo_common::serialization;
use itertools::{izip, zip_eq};
use rand_core::RngCore;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_with::serde_as;

use crate::{
    check_ciphertext_validity, generate_random, Ciphertext, PrivateKeyShare,
    PublicDecryptionContextFast, PublicDecryptionContextSimple,
};

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecryptionShareFast<E: Pairing> {
    pub decrypter_index: usize,
    #[serde_as(as = "serialization::SerdeAs")]
    pub decryption_share: E::G1Affine,
}

impl<E: Pairing> DecryptionShareFast<E> {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(&self).unwrap()
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        bincode::deserialize(bytes).unwrap()
    }
}

#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ValidatorShareChecksum<E: Pairing> {
    #[serde_as(as = "serialization::SerdeAs")]
    pub checksum: E::G1Affine,
}

impl<E: Pairing> ValidatorShareChecksum<E> {
    pub fn new(
        validator_decryption_key: &E::ScalarField,
        ciphertext: &Ciphertext<E>,
    ) -> Self {
        // C_i = dk_i^{-1} * U
        let checksum = ciphertext
            .commitment
            .mul(validator_decryption_key.inverse().unwrap())
            .into_affine();
        Self { checksum }
    }

    pub fn verify(
        &self,
        decryption_share: &E::TargetField,
        share_aggregate: &E::G2Affine,
        validator_public_key: &E::G2Affine,
        h: &E::G2,
        ciphertext: &Ciphertext<E>,
    ) -> bool {
        // D_i == e(C_i, Y_i)
        if *decryption_share != E::pairing(self.checksum, *share_aggregate).0 {
            return false;
        }

        // e(C_i, ek_i) == e(U, H)
        if E::pairing(self.checksum, *validator_public_key)
            != E::pairing(ciphertext.commitment, *h)
        {
            return false;
        }

        true
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        bincode::deserialize(bytes).unwrap()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(&self).unwrap()
    }
}

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "ValidatorShareChecksum<E>: Serialize",
    deserialize = "ValidatorShareChecksum<E>: DeserializeOwned"
))]
pub struct DecryptionShareSimple<E: Pairing> {
    // TODO: Add decryptor public key? Replace decryptor_index with public key?
    pub decrypter_index: usize,
    #[serde_as(as = "serialization::SerdeAs")]
    pub decryption_share: E::TargetField,
    pub validator_checksum: ValidatorShareChecksum<E>,
}

impl<E: Pairing> DecryptionShareSimple<E> {
    /// Create a decryption share from the given parameters.
    /// This function checks that the ciphertext is valid.
    pub fn create(
        validator_index: usize,
        validator_decryption_key: &E::ScalarField,
        private_key_share: &PrivateKeyShare<E>,
        ciphertext: &Ciphertext<E>,
        aad: &[u8],
        g_inv: &E::G1Prepared,
    ) -> Result<Self> {
        check_ciphertext_validity::<E>(ciphertext, aad, g_inv)?;
        Ok(Self::create_unchecked(
            validator_index,
            validator_decryption_key,
            private_key_share,
            ciphertext,
        ))
    }

    /// Create a decryption share from the given parameters.
    /// This function does not check that the ciphertext is valid.
    pub fn create_unchecked(
        validator_index: usize,
        validator_decryption_key: &E::ScalarField,
        private_key_share: &PrivateKeyShare<E>,
        ciphertext: &Ciphertext<E>,
    ) -> Self {
        // D_i = e(U, Z_i)
        let decryption_share = E::pairing(
            ciphertext.commitment,
            private_key_share.private_key_share,
        )
        .0;

        let validator_checksum =
            ValidatorShareChecksum::new(validator_decryption_key, ciphertext);

        Self {
            decrypter_index: validator_index,
            decryption_share,
            validator_checksum,
        }
    }
    /// Verify that the decryption share is valid.
    pub fn verify(
        &self,
        share_aggregate: &E::G2Affine,
        validator_public_key: &E::G2Affine,
        h: &E::G2,
        ciphertext: &Ciphertext<E>,
    ) -> bool {
        self.validator_checksum.verify(
            &self.decryption_share,
            share_aggregate,
            validator_public_key,
            h,
            ciphertext,
        )
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        bincode::deserialize(bytes).unwrap()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(&self).unwrap()
    }
}

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "ValidatorShareChecksum<E>: Serialize",
    deserialize = "ValidatorShareChecksum<E>: DeserializeOwned"
))]
pub struct DecryptionShareSimplePrecomputed<E: Pairing> {
    pub decrypter_index: usize,
    #[serde_as(as = "serialization::SerdeAs")]
    pub decryption_share: E::TargetField,
    pub validator_checksum: ValidatorShareChecksum<E>,
}

impl<E: Pairing> DecryptionShareSimplePrecomputed<E> {
    pub fn new(
        validator_index: usize,
        validator_decryption_key: &E::ScalarField,
        private_key_share: &PrivateKeyShare<E>,
        ciphertext: &Ciphertext<E>,
        aad: &[u8],
        lagrange_coeff: &E::ScalarField,
        g_inv: &E::G1Prepared,
    ) -> Result<Self> {
        check_ciphertext_validity::<E>(ciphertext, aad, g_inv)?;

        Ok(Self::create_unchecked(
            validator_index,
            validator_decryption_key,
            private_key_share,
            ciphertext,
            lagrange_coeff,
        ))
    }

    pub fn create_unchecked(
        validator_index: usize,
        validator_decryption_key: &E::ScalarField,
        private_key_share: &PrivateKeyShare<E>,
        ciphertext: &Ciphertext<E>,
        lagrange_coeff: &E::ScalarField,
    ) -> Self {
        // U_{λ_i} = [λ_{i}(0)] U
        let u_to_lagrange_coeff = ciphertext.commitment.mul(lagrange_coeff);
        // C_{λ_i} = e(U_{λ_i}, Z_i)
        let decryption_share = E::pairing(
            u_to_lagrange_coeff,
            private_key_share.private_key_share,
        )
        .0;

        let validator_checksum =
            ValidatorShareChecksum::new(validator_decryption_key, ciphertext);

        Self {
            decrypter_index: validator_index,
            decryption_share,
            validator_checksum,
        }
    }

    /// Verify that the decryption share is valid.
    pub fn verify(
        &self,
        share_aggregate: &E::G2Affine,
        validator_public_key: &E::G2Affine,
        h: &E::G2,
        ciphertext: &Ciphertext<E>,
    ) -> bool {
        self.validator_checksum.verify(
            &self.decryption_share,
            share_aggregate,
            validator_public_key,
            h,
            ciphertext,
        )
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        bincode::deserialize(bytes).unwrap()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }
}

// TODO: Remove this code? Currently only used in benchmarks. Move to benchmark suite?
pub fn batch_verify_decryption_shares<R: RngCore, E: Pairing>(
    pub_contexts: &[PublicDecryptionContextFast<E>],
    ciphertexts: &[Ciphertext<E>],
    decryption_shares: &[Vec<DecryptionShareFast<E>>],
    rng: &mut R,
) -> bool {
    let num_ciphertexts = ciphertexts.len();
    let num_shares = decryption_shares[0].len();

    // Get [b_i] H for each of the decryption shares
    let blinding_keys = decryption_shares[0]
        .iter()
        .map(|d| {
            pub_contexts[d.decrypter_index]
                .blinded_key_share
                .blinding_key_prepared
                .clone()
        })
        .collect::<Vec<_>>();

    // For each ciphertext, generate num_shares random scalars
    let alpha_ij = (0..num_ciphertexts)
        .map(|_| generate_random::<_, E>(num_shares, rng))
        .collect::<Vec<_>>();

    let mut pairings_a = Vec::with_capacity(num_shares + 1);
    let mut pairings_b = Vec::with_capacity(num_shares + 1);

    // Compute \sum_j \alpha_{i,j} for each ciphertext i
    let sum_alpha_i = alpha_ij
        .iter()
        .map(|alpha_j| alpha_j.iter().sum::<E::ScalarField>())
        .collect::<Vec<_>>();

    // Compute \sum_i [ \sum_j \alpha_{i,j} ] U_i
    let sum_u_i = E::G1Prepared::from(
        izip!(ciphertexts.iter(), sum_alpha_i.iter())
            .map(|(c, alpha_j)| c.commitment.mul(*alpha_j))
            .sum::<E::G1>()
            .into_affine(),
    );

    // e(\sum_i [ \sum_j \alpha_{i,j} ] U_i, -H)
    pairings_a.push(sum_u_i);
    pairings_b.push(pub_contexts[0].h_inv.clone());

    let mut sum_d_i = vec![E::G1::zero(); num_shares];

    // sum_D_i = { [\sum_i \alpha_{i,j} ] D_i }
    for (d, alpha_j) in izip!(decryption_shares.iter(), alpha_ij.iter()) {
        for (sum_alpha_d_i, d_ij, alpha) in
            izip!(sum_d_i.iter_mut(), d.iter(), alpha_j.iter())
        {
            *sum_alpha_d_i += d_ij.decryption_share.mul(*alpha);
        }
    }

    // e([\sum_i \alpha_{i,j} ] D_i, B_i)
    for (d_i, b_i) in izip!(sum_d_i.iter(), blinding_keys.iter()) {
        pairings_a.push(E::G1Prepared::from(d_i.into_affine()));
        pairings_b.push(b_i.clone());
    }

    E::multi_pairing(pairings_a, pairings_b).0 == E::TargetField::one()
}

pub fn verify_decryption_shares_fast<E: Pairing>(
    pub_contexts: &[PublicDecryptionContextFast<E>],
    ciphertext: &Ciphertext<E>,
    decryption_shares: &[DecryptionShareFast<E>],
) -> bool {
    // [b_i] H
    let blinding_keys = decryption_shares
        .iter()
        .map(|d| {
            pub_contexts[d.decrypter_index]
                .blinded_key_share
                .blinding_key_prepared
                .clone()
        })
        .collect::<Vec<_>>();

    let mut pairing_a: Vec<E::G1Prepared> = vec![];
    let mut pairing_b = vec![];

    // e(U, -H)
    pairing_a.push(ciphertext.commitment.into());
    pairing_b.push(pub_contexts[0].h_inv.clone());

    for (d_i, p_i) in zip_eq(decryption_shares, blinding_keys) {
        let mut pairing_a_i = pairing_a.clone();
        let mut pairing_b_i = pairing_b.clone();
        // e(D_i, B_i)
        pairing_a_i.push(d_i.decryption_share.into());
        pairing_b_i.push(p_i.clone());
        if E::multi_pairing(pairing_a_i, pairing_b_i).0 != E::TargetField::one()
        {
            return false;
        }
    }

    true
}

pub fn verify_decryption_shares_simple<E: Pairing>(
    pub_contexts: &Vec<PublicDecryptionContextSimple<E>>,
    ciphertext: &Ciphertext<E>,
    decryption_shares: &Vec<DecryptionShareSimple<E>>,
) -> bool {
    let blinded_key_shares = &pub_contexts
        .iter()
        .map(|c| &c.blinded_key_share.blinded_key_share)
        .collect::<Vec<_>>();
    for (decryption_share, y_i, pub_context) in
        izip!(decryption_shares, blinded_key_shares, pub_contexts)
    {
        let is_valid = decryption_share.verify(
            y_i,
            &pub_context.validator_public_key.into_affine(),
            &pub_context.h.into(),
            ciphertext,
        );
        if !is_valid {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use ark_ec::AffineRepr;

    use crate::*;

    type E = ark_bls12_381::Bls12_381;

    #[test]
    fn decryption_share_serialization() {
        let decryption_share = DecryptionShareFast::<E> {
            decrypter_index: 1,
            decryption_share: ark_bls12_381::G1Affine::generator(),
        };

        let serialized = decryption_share.to_bytes();
        let deserialized: DecryptionShareFast<E> =
            DecryptionShareFast::from_bytes(&serialized);
        assert_eq!(serialized, deserialized.to_bytes())
    }
}
