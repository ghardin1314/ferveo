use std::{ops::Mul, usize};

use ark_ec::{pairing::Pairing, AffineRepr, CurveGroup};
use ark_ff::Zero;
use ark_poly::{univariate::DensePolynomial, DenseUVPolynomial, Polynomial};
use ferveo_tdec::{lagrange_basis_at, PrivateKeyShare};
use itertools::zip_eq;
use rand_core::RngCore;

// SHARE UPDATE FUNCTIONS:

/// From PSS paper, section 4.2.1, (https://link.springer.com/content/pdf/10.1007/3-540-44750-4_27.pdf)
pub fn prepare_share_updates_for_recovery<E: Pairing>(
    domain_points: &[E::ScalarField],
    h: &E::G2Affine,
    x_r: &E::ScalarField,
    threshold: usize,
    rng: &mut impl RngCore,
) -> Vec<E::G2> {
    // Update polynomial has root at x_r
    prepare_share_updates_with_root::<E>(domain_points, h, x_r, threshold, rng)
}

// TODO: Consider relocating to PrivateKeyShare (see #162, #163)
/// From PSS paper, section 4.2.3, (https://link.springer.com/content/pdf/10.1007/3-540-44750-4_27.pdf)
pub fn apply_updates_to_private_share<E: Pairing>(
    private_key_share: &PrivateKeyShare<E>,
    share_updates: &[E::G2],
) -> PrivateKeyShare<E> {
    let private_key_share = share_updates
        .iter()
        .fold(
            private_key_share.private_key_share.into_group(),
            |acc, delta| acc + delta,
        )
        .into_affine();
    PrivateKeyShare { private_key_share }
}

/// From the PSS paper, section 4.2.4, (https://link.springer.com/content/pdf/10.1007/3-540-44750-4_27.pdf)
pub fn recover_share_from_updated_private_shares<E: Pairing>(
    x_r: &E::ScalarField,
    domain_points: &[E::ScalarField],
    updated_private_shares: &[PrivateKeyShare<E>],
) -> PrivateKeyShare<E> {
    // Interpolate new shares to recover y_r
    let lagrange = lagrange_basis_at::<E>(domain_points, x_r);
    let prods = zip_eq(updated_private_shares, lagrange)
        .map(|(y_j, l)| y_j.private_key_share.mul(l));
    let y_r = prods.fold(E::G2::zero(), |acc, y_j| acc + y_j);

    PrivateKeyShare {
        private_key_share: y_r.into_affine(),
    }
}

// SHARE REFRESH FUNCTIONS:

pub fn prepare_share_updates_for_refresh<E: Pairing>(
    domain_points: &[E::ScalarField],
    h: &E::G2Affine,
    threshold: usize,
    rng: &mut impl RngCore,
) -> Vec<E::G2> {
    // Update polynomial has root at 0
    prepare_share_updates_with_root::<E>(
        domain_points,
        h,
        &E::ScalarField::zero(),
        threshold,
        rng,
    )
}

// UTILS:

fn prepare_share_updates_with_root<E: Pairing>(
    domain_points: &[E::ScalarField],
    h: &E::G2Affine,
    root: &E::ScalarField,
    threshold: usize,
    rng: &mut impl RngCore,
) -> Vec<E::G2> {
    // Generate a new random polynomial with defined root
    let d_i = make_random_polynomial_with_root::<E>(threshold - 1, root, rng);

    // Now, we need to evaluate the polynomial at each of participants' indices
    domain_points
        .iter()
        .map(|x_i| {
            let eval = d_i.evaluate(x_i);
            h.mul(eval)
        })
        .collect()
}

pub fn make_random_polynomial_with_root<E: Pairing>(
    degree: usize,
    root: &E::ScalarField,
    rng: &mut impl RngCore,
) -> DensePolynomial<E::ScalarField> {
    // [c_0, c_1, ..., c_{degree}] (Random polynomial)
    let mut poly = DensePolynomial::<E::ScalarField>::rand(degree, rng);

    // [0, c_1, ... , c_{degree}]  (We zeroize the free term)
    poly[0] = E::ScalarField::zero();

    // Now, we calculate a new free term so that `poly(root) = 0`
    let new_c_0 = E::ScalarField::zero() - poly.evaluate(root);
    poly[0] = new_c_0;

    // Evaluating the polynomial at the root should result in 0
    debug_assert!(poly.evaluate(root) == E::ScalarField::zero());
    debug_assert!(poly.coeffs.len() == degree + 1);

    poly
}

#[cfg(test)]
mod tests_refresh {

    use std::collections::HashMap;

    use ark_bls12_381::Fr;
    use ark_std::{test_rng, UniformRand, Zero};
    use ferveo_tdec::{
        test_common::setup_simple, PrivateDecryptionContextSimple,
        PrivateKeyShare,
    };
    use rand_core::RngCore;
    use test_case::test_matrix;

    use crate::{
        apply_updates_to_private_share, prepare_share_updates_for_recovery,
        prepare_share_updates_for_refresh,
        recover_share_from_updated_private_shares, test_common::*,
    };

    fn make_new_share_fragments_for_recovery<R: RngCore>(
        rng: &mut R,
        threshold: usize,
        x_r: &Fr,
        remaining_participants: &[PrivateDecryptionContextSimple<E>],
    ) -> Vec<PrivateKeyShare<E>> {
        // Each participant prepares an update for each other participant
        // TODO: Extract as parameter
        let domain_points = remaining_participants[0]
            .public_decryption_contexts
            .iter()
            .map(|c| c.domain)
            .collect::<Vec<_>>();
        let h = remaining_participants[0].public_decryption_contexts[0].h;
        let share_updates = remaining_participants
            .iter()
            .map(|p| {
                let deltas_i = prepare_share_updates_for_recovery::<E>(
                    &domain_points,
                    &h,
                    x_r,
                    threshold,
                    rng,
                );
                (p.index, deltas_i)
            })
            .collect::<HashMap<_, _>>();

        // Participants share updates and update their shares
        let new_share_fragments: Vec<_> = remaining_participants
            .iter()
            .map(|p| {
                // Current participant receives updates from other participants
                let updates_for_participant: Vec<_> = share_updates
                    .values()
                    .map(|updates| *updates.get(p.index).unwrap())
                    .collect();

                // And updates their share
                apply_updates_to_private_share::<E>(
                    &p.private_key_share,
                    &updates_for_participant,
                )
            })
            .collect();

        new_share_fragments
    }

    /// Ñ parties (where t <= Ñ <= N) jointly execute a "share recovery" algorithm, and the output is 1 new share.
    /// The new share is intended to restore a previously existing share, e.g., due to loss or corruption.
    #[test_matrix([4, 7, 11, 16])]
    fn tdec_simple_variant_share_recovery_at_selected_point(shares_num: usize) {
        let rng = &mut test_rng();
        let security_threshold = shares_num * 2 / 3;

        let (_, _, mut contexts) =
            setup_simple::<E>(security_threshold, shares_num, rng);

        // Prepare participants

        // First, save the soon-to-be-removed participant
        let selected_participant = contexts.pop().unwrap();
        let x_r = selected_participant
            .public_decryption_contexts
            .last()
            .unwrap()
            .domain;
        let original_private_key_share = selected_participant.private_key_share;

        // Remove one participant from the contexts and all nested structures
        let mut remaining_participants = contexts;
        for p in &mut remaining_participants {
            p.public_decryption_contexts.pop().unwrap();
        }

        // Each participant prepares an update for each other participant, and uses it to create a new share fragment
        let new_share_fragments = make_new_share_fragments_for_recovery(
            rng,
            security_threshold,
            &x_r,
            &remaining_participants,
        );

        // Now, we have to combine new share fragments into a new share
        let domain_points = &remaining_participants[0]
            .public_decryption_contexts
            .iter()
            .map(|ctxt| ctxt.domain)
            .collect::<Vec<_>>();
        let new_private_key_share = recover_share_from_updated_private_shares(
            &x_r,
            &domain_points[..security_threshold],
            &new_share_fragments[..security_threshold],
        );

        assert_eq!(new_private_key_share, original_private_key_share);

        // If we don't have enough private share updates, the resulting private share will be incorrect
        assert_eq!(domain_points.len(), new_share_fragments.len());
        let incorrect_private_key_share =
            recover_share_from_updated_private_shares(
                &x_r,
                &domain_points[..(security_threshold - 1)],
                &new_share_fragments[..(security_threshold - 1)],
            );

        assert_ne!(incorrect_private_key_share, original_private_key_share);
    }

    /// Ñ parties (where t <= Ñ <= N) jointly execute a "share recovery" algorithm, and the output is 1 new share.
    /// The new share is independent from the previously existing shares. We can use this to on-board a new participant into an existing cohort.
    #[test_matrix([4, 7, 11, 16])]
    fn tdec_simple_variant_share_recovery_at_random_point(shares_num: usize) {
        let rng = &mut test_rng();
        let threshold = shares_num * 2 / 3;

        let (_, shared_private_key, mut contexts) =
            setup_simple::<E>(threshold, shares_num, rng);

        // Prepare participants

        // Remove one participant from the contexts and all nested structures
        contexts.pop().unwrap();
        let mut remaining_participants = contexts.clone();
        for p in &mut remaining_participants {
            p.public_decryption_contexts.pop().unwrap();
        }

        // Now, we're going to recover a new share at a random point and check that the shared secret is still the same

        // Our random point
        let x_r = ScalarField::rand(rng);

        // Each participant prepares an update for each other participant, and uses it to create a new share fragment
        let new_share_fragments = make_new_share_fragments_for_recovery(
            rng,
            threshold,
            &x_r,
            &remaining_participants,
        );

        // Now, we have to combine new share fragments into a new share
        let domain_points = &mut remaining_participants[0]
            .public_decryption_contexts
            .iter()
            .map(|ctxt| ctxt.domain)
            .collect::<Vec<_>>();
        let new_private_key_share = recover_share_from_updated_private_shares(
            &x_r,
            &domain_points[..threshold],
            &new_share_fragments[..threshold],
        );

        let mut private_shares = contexts
            .iter()
            .cloned()
            .map(|ctxt| ctxt.private_key_share)
            .collect::<Vec<_>>();

        // Finally, let's recreate the shared private key from some original shares and the recovered one
        domain_points.push(x_r);
        private_shares.push(new_private_key_share);
        let start_from = shares_num - threshold;
        let new_shared_private_key = recover_share_from_updated_private_shares(
            &ScalarField::zero(),
            &domain_points[start_from..],
            &private_shares[start_from..],
        );

        assert_eq!(
            shared_private_key,
            new_shared_private_key.private_key_share
        );
    }

    /// Ñ parties (where t <= Ñ <= N) jointly execute a "share refresh" algorithm.
    /// The output is M new shares (with M <= Ñ), with each of the M new shares substituting the
    /// original share (i.e., the original share is deleted).
    #[test_matrix([4, 7, 11, 16])]

    fn tdec_simple_variant_share_refreshing(shares_num: usize) {
        let rng = &mut test_rng();
        let threshold = shares_num * 2 / 3;

        let (_, shared_private_key, contexts) =
            setup_simple::<E>(threshold, shares_num, rng);

        let domain_points = &contexts[0]
            .public_decryption_contexts
            .iter()
            .map(|ctxt| ctxt.domain)
            .collect::<Vec<_>>();
        let h = contexts[0].public_decryption_contexts[0].h;

        // Each participant prepares an update for each other participant:
        let share_updates = contexts
            .iter()
            .map(|p| {
                let deltas_i = prepare_share_updates_for_refresh::<E>(
                    domain_points,
                    &h,
                    threshold,
                    rng,
                );
                (p.index, deltas_i)
            })
            .collect::<HashMap<_, _>>();

        // Participants "refresh" their shares with the updates from each other:
        let refreshed_shares: Vec<_> = contexts
            .iter()
            .map(|p| {
                // Current participant receives updates from other participants
                let updates_for_participant: Vec<_> = share_updates
                    .values()
                    .map(|updates| *updates.get(p.index).unwrap())
                    .collect();

                // And updates their share
                apply_updates_to_private_share::<E>(
                    &p.private_key_share,
                    &updates_for_participant,
                )
            })
            .collect();

        // Finally, let's recreate the shared private key from the refreshed shares
        let new_shared_private_key = recover_share_from_updated_private_shares(
            &ScalarField::zero(),
            &domain_points[..threshold],
            &refreshed_shares[..threshold],
        );

        assert_eq!(
            shared_private_key,
            new_shared_private_key.private_key_share
        );
    }
}
