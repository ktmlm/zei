use crate::anon_xfr::{
    anonymous_transfer::TurboPlonkCS,
    keys::AXfrPubKey,
    proofs::AXfrPlonkPf,
    structs::{AnonBlindAssetRecord, OpenAnonBlindAssetRecord, OpenAnonBlindAssetRecordBuilder},
};
use crate::setup::{ProverParams, VerifierParams};
use crate::xfr::{
    sig::{XfrKeyPair, XfrPublicKey, XfrSignature},
    structs::{BlindAssetRecord, OpenAssetRecord, OwnerMemo, XfrAmount, XfrAssetType},
};
use merlin::Transcript;
use num_bigint::BigUint;
use zei_algebra::{bls12_381::BLSScalar, prelude::*, ristretto::RistrettoScalar};
use zei_crypto::{
    basic::{
        hybrid_encryption::XPublicKey, rescue::RescueInstance,
        ristretto_pedersen_comm::RistrettoPedersenCommitment,
    },
    delegated_chaum_pedersen::{
        prove_delegated_chaum_pedersen, verify_delegated_chaum_pedersen, NonZKState, ZKPartProof,
    },
    field_simulation::{SimFr, BIT_PER_LIMB, NUM_OF_LIMBS},
};
use zei_plonk::plonk::{
    constraint_system::{field_simulation::SimFrVar, rescue::StateVar, TurboCS},
    prover::prover_with_lagrange,
    verifier::verifier,
};

const BAR_TO_ABAR_TRANSCRIPT: &[u8] = b"BAR to ABAR proof";
pub const TWO_POW_32: u64 = 1 << 32;

#[derive(Debug, Serialize, Deserialize, Eq, Clone, PartialEq)]
pub struct ConvertBarAbarProof {
    commitment_eq_proof: ZKPartProof,
    pc_rescue_commitments_eq_proof: AXfrPlonkPf,
}

#[derive(Debug, Serialize, Deserialize, Eq, Clone, PartialEq)]
pub struct BarToAbarBody {
    pub input: BlindAssetRecord,
    pub output: AnonBlindAssetRecord,
    pub proof: ConvertBarAbarProof,
    pub memo: OwnerMemo,
}

#[derive(Debug, Serialize, Deserialize, Eq, Clone, PartialEq)]
pub struct BarToAbarNote {
    pub body: BarToAbarBody,
    pub signature: XfrSignature,
}

/// Generate Bar To Abar conversion note body
/// Returns note Body and ABAR opening keys
pub fn gen_bar_to_abar_body<R: CryptoRng + RngCore>(
    prng: &mut R,
    params: &ProverParams,
    record: &OpenAssetRecord,
    abar_pubkey: &AXfrPubKey,
    enc_key: &XPublicKey,
) -> Result<BarToAbarBody> {
    let (open_abar, proof) = bar_to_abar(prng, params, record, abar_pubkey, enc_key).c(d!())?;
    let body = BarToAbarBody {
        input: record.blind_asset_record.clone(),
        output: AnonBlindAssetRecord::from_oabar(&open_abar),
        proof,
        memo: open_abar.owner_memo.unwrap(),
    };
    Ok(body)
}

/// Generate BlindAssetRecord To AnonymousBlindAssetRecord conversion note: body + spending input signature
/// Returns conversion note
pub fn gen_bar_to_abar_note<R: CryptoRng + RngCore>(
    prng: &mut R,
    params: &ProverParams,
    record: &OpenAssetRecord,
    bar_keypair: &XfrKeyPair,
    abar_pubkey: &AXfrPubKey,
    enc_key: &XPublicKey,
) -> Result<BarToAbarNote> {
    let body = gen_bar_to_abar_body(prng, params, record, &abar_pubkey, enc_key).c(d!())?;
    let msg = bincode::serialize(&body)
        .map_err(|_| ZeiError::SerializationError)
        .c(d!())?;
    let signature = bar_keypair.sign(&msg);
    let note = BarToAbarNote { body, signature };
    Ok(note)
}

/// Verifies BlindAssetRecord To AnonymousBlindAssetRecord conversion body
/// Warning: This function doesn't check that input owner has signed the body
pub fn verify_bar_to_abar_body(params: &VerifierParams, body: &BarToAbarBody) -> Result<()> {
    verify_bar_to_abar(params, &body.input, &body.output, &body.proof).c(d!())
}

/// Verifies BlindAssetRecord To AnonymousBlindAssetRecord conversion note by verifying proof of conversion
/// and signature by input owner key
pub fn verify_bar_to_abar_note(
    params: &VerifierParams,
    note: &BarToAbarNote,
    bar_pub_key: &XfrPublicKey,
) -> Result<()> {
    verify_bar_to_abar_body(params, &note.body).c(d!())?;
    let msg = bincode::serialize(&note.body).c(d!(ZeiError::SerializationError))?;
    bar_pub_key.verify(&msg, &note.signature).c(d!())
}

pub(crate) fn bar_to_abar<R: CryptoRng + RngCore>(
    prng: &mut R,
    params: &ProverParams,
    obar: &OpenAssetRecord,
    abar_pubkey: &AXfrPubKey,
    enc_key: &XPublicKey,
) -> Result<(OpenAnonBlindAssetRecord, ConvertBarAbarProof)> {
    let oabar_amount = obar.amount;

    let pc_gens = RistrettoPedersenCommitment::default();

    // 1. Construct ABAR.
    let oabar = OpenAnonBlindAssetRecordBuilder::new()
        .amount(oabar_amount)
        .asset_type(obar.asset_type)
        .pub_key(*abar_pubkey)
        .finalize(prng, &enc_key)
        .c(d!())?
        .build()
        .c(d!())?;

    // 2. Reconstruct the points.
    let x = RistrettoScalar::from(oabar_amount);
    let y: RistrettoScalar = obar.asset_type.as_scalar();
    let gamma = obar
        .amount_blinds
        .0
        .add(&obar.amount_blinds.1.mul(&RistrettoScalar::from(TWO_POW_32)));
    let delta = obar.type_blind;
    let point_p = pc_gens.commit(x, gamma);
    let point_q = pc_gens.commit(y, delta);

    let z_randomizer = oabar.blind;
    let z_instance = RescueInstance::<BLSScalar>::new();

    let x_in_bls12_381 = BLSScalar::from(&BigUint::from_bytes_le(&x.to_bytes()));
    let y_in_bls12_381 = BLSScalar::from(&BigUint::from_bytes_le(&y.to_bytes()));

    let z = {
        let cur = z_instance.rescue(&[
            z_randomizer,
            x_in_bls12_381,
            y_in_bls12_381,
            BLSScalar::zero(),
        ])[0];
        z_instance.rescue(&[
            cur,
            abar_pubkey.0.point_ref().get_x(),
            BLSScalar::zero(),
            BLSScalar::zero(),
        ])[0]
    };

    // 3. compute the non-ZK part of the proof
    let (commitment_eq_proof, non_zk_state, beta, lambda) = prove_delegated_chaum_pedersen(
        prng, &x, &gamma, &y, &delta, &pc_gens, &point_p, &point_q, &z,
    )
    .c(d!())?;

    // 4. prove abar correctness
    let pc_rescue_commitments_eq_proof = prove_eq_committed_vals(
        prng,
        params,
        x_in_bls12_381,
        y_in_bls12_381,
        oabar.blind,
        abar_pubkey.0.point_ref().get_x(),
        &commitment_eq_proof,
        &non_zk_state,
        &beta,
        &lambda,
    )
    .c(d!())?;

    Ok((
        oabar,
        ConvertBarAbarProof {
            commitment_eq_proof,
            pc_rescue_commitments_eq_proof,
        },
    ))
}

pub(crate) fn verify_bar_to_abar(
    params: &VerifierParams,
    bar: &BlindAssetRecord,
    abar: &AnonBlindAssetRecord,
    proof: &ConvertBarAbarProof,
) -> Result<()> {
    let pc_gens = RistrettoPedersenCommitment::default();

    // 1. get commitments
    // 1.1 reconstruct total amount commitment from bar object
    let (com_low, com_high) = match bar.amount {
        XfrAmount::Confidential((low, high)) => (
            low.decompress()
                .ok_or(ZeiError::DecompressElementError)
                .c(d!())?,
            high.decompress()
                .ok_or(ZeiError::DecompressElementError)
                .c(d!())?,
        ),
        XfrAmount::NonConfidential(amount) => {
            // fake commitment
            let (l, h) = u64_to_u32_pair(amount);
            (
                pc_gens.commit(RistrettoScalar::from(l), RistrettoScalar::zero()),
                pc_gens.commit(RistrettoScalar::from(h), RistrettoScalar::zero()),
            )
        }
    };

    // 1.2 get asset type commitment
    let com_amount = com_low.add(&com_high.mul(&RistrettoScalar::from(TWO_POW_32)));
    let com_asset_type = match bar.asset_type {
        XfrAssetType::Confidential(a) => a
            .decompress()
            .ok_or(ZeiError::DecompressElementError)
            .c(d!())?,
        XfrAssetType::NonConfidential(a) => {
            // fake commitment
            pc_gens.commit(a.as_scalar(), RistrettoScalar::zero())
        }
    };

    // 2. verify equality of committed values
    let (beta, lambda) = verify_delegated_chaum_pedersen(
        &pc_gens,
        &com_amount,
        &com_asset_type,
        &abar.commitment,
        &proof.commitment_eq_proof,
    )
    .c(d!())?;

    // 3. verify PLONK proof
    verify_eq_committed_vals(
        params,
        abar.commitment,
        &proof.commitment_eq_proof,
        &proof.pc_rescue_commitments_eq_proof,
        &beta,
        &lambda,
    )
    .c(d!())
}

/// Generate the plonk proof for equality of values in a Pedersen commitment and a Rescue commitment.
/// * `rng` - pseudo-random generator.
/// * `params` - System params
/// * `amount` - transaction amount
/// * `asset_type` - asset type
/// * `blind_pc` - blinding factor for the Pedersen commitment
/// * `blind_hash` - blinding factor for the Rescue commitment
/// * `pc_gens` - the Pedersen commitment instance
/// * Return the plonk proof if the witness is valid, return an error otherwise.
pub(crate) fn prove_eq_committed_vals<R: CryptoRng + RngCore>(
    rng: &mut R,
    params: &ProverParams,
    amount: BLSScalar,
    asset_type: BLSScalar,
    blind_hash: BLSScalar,
    pubkey_x: BLSScalar,
    proof: &ZKPartProof,
    non_zk_state: &NonZKState,
    beta: &RistrettoScalar,
    lambda: &RistrettoScalar,
) -> Result<AXfrPlonkPf> {
    let mut transcript = Transcript::new(BAR_TO_ABAR_TRANSCRIPT);
    let (mut cs, _) = build_bar_to_abar_cs(
        amount,
        asset_type,
        blind_hash,
        pubkey_x,
        proof,
        non_zk_state,
        beta,
        lambda,
    );
    let witness = cs.get_and_clear_witness();

    prover_with_lagrange(
        rng,
        &mut transcript,
        &params.pcs,
        params.lagrange_pcs.as_ref(),
        &params.cs,
        &params.prover_params,
        &witness,
    )
    .c(d!(ZeiError::AXfrProofError))
}

/// I verify the plonk proof for equality of values in a Pedersen commitment and a Rescue commitment.
/// * `params` - System parameters including KZG params and the constraint system
/// * `hash_comm` - the Rescue commitment
/// * `ped_comm` - the Pedersen commitment
/// * `proof` - the proof
/// * Returns Ok() if the verification succeeds, returns an error otherwise.
pub(crate) fn verify_eq_committed_vals(
    params: &VerifierParams,
    hash_comm: BLSScalar,
    proof_zk_part: &ZKPartProof,
    proof: &AXfrPlonkPf,
    beta: &RistrettoScalar,
    lambda: &RistrettoScalar,
) -> Result<()> {
    let mut transcript = Transcript::new(BAR_TO_ABAR_TRANSCRIPT);
    let mut online_inputs = Vec::with_capacity(2 + 3 * NUM_OF_LIMBS);
    online_inputs.push(hash_comm);
    online_inputs.push(proof_zk_part.non_zk_part_state_commitment);
    let beta_sim_fr = SimFr::from(&BigUint::from_bytes_le(&beta.to_bytes()));
    let lambda_sim_fr = SimFr::from(&BigUint::from_bytes_le(&lambda.to_bytes()));

    let beta_lambda = *beta * lambda;
    let beta_lambda_sim_fr = SimFr::from(&BigUint::from_bytes_le(&beta_lambda.to_bytes()));

    let s1_plus_lambda_s2 = proof_zk_part.s_1 + proof_zk_part.s_2 * lambda;
    let s1_plus_lambda_s2_sim_fr =
        SimFr::from(&BigUint::from_bytes_le(&s1_plus_lambda_s2.to_bytes()));

    online_inputs.extend_from_slice(&beta_sim_fr.limbs);
    online_inputs.extend_from_slice(&lambda_sim_fr.limbs);
    online_inputs.extend_from_slice(&beta_lambda_sim_fr.limbs);
    online_inputs.extend_from_slice(&s1_plus_lambda_s2_sim_fr.limbs);

    verifier(
        &mut transcript,
        &params.pcs,
        &params.cs,
        &params.verifier_params,
        &online_inputs,
        proof,
    )
    .c(d!(ZeiError::ZKProofVerificationError))
}

/// Returns the constraint system (and associated number of constraints) for equality of values
/// in a Pedersen commitment and a Rescue commitment.
pub(crate) fn build_bar_to_abar_cs(
    amount: BLSScalar,
    asset_type: BLSScalar,
    blind_hash: BLSScalar,
    pubkey_x: BLSScalar,
    proof: &ZKPartProof,
    non_zk_state: &NonZKState,
    beta: &RistrettoScalar,
    lambda: &RistrettoScalar,
) -> (TurboPlonkCS, usize) {
    let mut cs = TurboCS::new();
    let zero_var = cs.zero_var();

    let zero = BLSScalar::zero();
    let one = BLSScalar::one();
    let step_1 = BLSScalar::from(&BigUint::one().shl(BIT_PER_LIMB));
    let step_2 = BLSScalar::from(&BigUint::one().shl(BIT_PER_LIMB * 2));
    let step_3 = BLSScalar::from(&BigUint::one().shl(BIT_PER_LIMB * 3));
    let step_4 = BLSScalar::from(&BigUint::one().shl(BIT_PER_LIMB * 4));
    let step_5 = BLSScalar::from(&BigUint::one().shl(BIT_PER_LIMB * 5));

    // 1. Input Ristretto commitment data
    let amount_var = cs.new_variable(amount);
    let at_var = cs.new_variable(asset_type);
    let blind_hash_var = cs.new_variable(blind_hash);
    let pubkey_x_var = cs.new_variable(pubkey_x);

    // 2. Input witness x, y, a, b, r, public input comm, beta, s1, s2
    let x_sim_fr = SimFr::from(&BigUint::from_bytes_le(&non_zk_state.x.to_bytes()));
    let y_sim_fr = SimFr::from(&BigUint::from_bytes_le(&non_zk_state.y.to_bytes()));
    let a_sim_fr = SimFr::from(&BigUint::from_bytes_le(&non_zk_state.a.to_bytes()));
    let b_sim_fr = SimFr::from(&BigUint::from_bytes_le(&non_zk_state.b.to_bytes()));
    let comm = proof.non_zk_part_state_commitment;
    let r = non_zk_state.r;

    let beta_sim_fr = SimFr::from(&BigUint::from_bytes_le(&beta.to_bytes()));
    let lambda_sim_fr = SimFr::from(&BigUint::from_bytes_le(&lambda.to_bytes()));

    let beta_lambda = *beta * lambda;
    let beta_lambda_sim_fr = SimFr::from(&BigUint::from_bytes_le(&beta_lambda.to_bytes()));

    let s1_plus_lambda_s2 = proof.s_1 + proof.s_2 * lambda;
    let s1_plus_lambda_s2_sim_fr =
        SimFr::from(&BigUint::from_bytes_le(&s1_plus_lambda_s2.to_bytes()));

    let x_sim_fr_var = SimFrVar::alloc_witness_bounded_total_bits(&mut cs, &x_sim_fr, 64);
    let y_sim_fr_var = SimFrVar::alloc_witness_bounded_total_bits(&mut cs, &y_sim_fr, 240);
    let a_sim_fr_var = SimFrVar::alloc_witness(&mut cs, &a_sim_fr);
    let b_sim_fr_var = SimFrVar::alloc_witness(&mut cs, &b_sim_fr);
    let comm_var = cs.new_variable(comm);
    let r_var = cs.new_variable(r);
    let beta_sim_fr_var = SimFrVar::alloc_input(&mut cs, &beta_sim_fr);
    let lambda_sim_fr_var = SimFrVar::alloc_input(&mut cs, &lambda_sim_fr);
    let beta_lambda_sim_fr_var = SimFrVar::alloc_input(&mut cs, &beta_lambda_sim_fr);
    let s1_plus_lambda_s2_sim_fr_var = SimFrVar::alloc_input(&mut cs, &s1_plus_lambda_s2_sim_fr);

    // 3. Merge the limbs for x, y, a, b
    let mut all_limbs = Vec::with_capacity(4 * NUM_OF_LIMBS);
    all_limbs.extend_from_slice(&x_sim_fr.limbs);
    all_limbs.extend_from_slice(&y_sim_fr.limbs);
    all_limbs.extend_from_slice(&a_sim_fr.limbs);
    all_limbs.extend_from_slice(&b_sim_fr.limbs);

    let mut all_limbs_var = Vec::with_capacity(4 * NUM_OF_LIMBS);
    all_limbs_var.extend_from_slice(&x_sim_fr_var.var);
    all_limbs_var.extend_from_slice(&y_sim_fr_var.var);
    all_limbs_var.extend_from_slice(&a_sim_fr_var.var);
    all_limbs_var.extend_from_slice(&b_sim_fr_var.var);

    let mut compressed_limbs = Vec::with_capacity(5);
    let mut compressed_limbs_var = Vec::with_capacity(5);
    for (limbs, limbs_var) in all_limbs.chunks(5).zip(all_limbs_var.chunks(5)) {
        let mut sum = BigUint::zero();
        for (i, limb) in limbs.iter().enumerate() {
            sum.add_assign(<&BLSScalar as Into<BigUint>>::into(limb).shl(BIT_PER_LIMB * i));
        }
        compressed_limbs.push(BLSScalar::from(&sum));

        let mut sum_var = {
            let first_var = *limbs_var.get(0).unwrap_or(&zero_var);
            let second_var = *limbs_var.get(1).unwrap_or(&zero_var);
            let third_var = *limbs_var.get(2).unwrap_or(&zero_var);
            let fourth_var = *limbs_var.get(3).unwrap_or(&zero_var);

            cs.linear_combine(
                &[first_var, second_var, third_var, fourth_var],
                one,
                step_1,
                step_2,
                step_3,
            )
        };

        if limbs.len() == 5 {
            let fifth_var = *limbs_var.get(4).unwrap_or(&zero_var);
            sum_var = cs.linear_combine(
                &[sum_var, fifth_var, zero_var, zero_var],
                one,
                step_4,
                zero,
                zero,
            );
        }

        compressed_limbs_var.push(sum_var);
    }

    // 4. Open the non-ZK verifier state
    {
        let h1_var = cs.rescue_hash(&StateVar::new([
            compressed_limbs_var[0],
            compressed_limbs_var[1],
            compressed_limbs_var[2],
            compressed_limbs_var[3],
        ]))[0];

        let h2_var = cs.rescue_hash(&StateVar::new([
            h1_var,
            compressed_limbs_var[4],
            r_var,
            zero_var,
        ]))[0];
        cs.equal(h2_var, comm_var);
    }

    // 5. Perform the check in field simulation
    {
        let beta_x_sim_fr_mul_var = beta_sim_fr_var.mul(&mut cs, &x_sim_fr_var);
        let beta_lambda_y_sim_fr_mul_var = beta_lambda_sim_fr_var.mul(&mut cs, &y_sim_fr_var);
        let lambda_b_sim_fr_mul_var = lambda_sim_fr_var.mul(&mut cs, &b_sim_fr_var);

        let mut rhs = beta_x_sim_fr_mul_var.add(&mut cs, &beta_lambda_y_sim_fr_mul_var);
        rhs = rhs.add(&mut cs, &lambda_b_sim_fr_mul_var);

        let s1_plus_lambda_s2_minus_a_sim_fr_var =
            s1_plus_lambda_s2_sim_fr_var.sub(&mut cs, &a_sim_fr_var);

        let eqn = rhs.sub(&mut cs, &s1_plus_lambda_s2_minus_a_sim_fr_var);
        eqn.enforce_zero(&mut cs);
    }

    // 6. Check x = amount_var and y = at_var
    {
        let mut x_in_bls12_381 = cs.linear_combine(
            &[
                x_sim_fr_var.var[0],
                x_sim_fr_var.var[1],
                x_sim_fr_var.var[2],
                x_sim_fr_var.var[3],
            ],
            one,
            step_1,
            step_2,
            step_3,
        );
        x_in_bls12_381 = cs.linear_combine(
            &[
                x_in_bls12_381,
                x_sim_fr_var.var[4],
                x_sim_fr_var.var[5],
                zero_var,
            ],
            one,
            step_4,
            step_5,
            zero,
        );

        let mut y_in_bls12_381 = cs.linear_combine(
            &[
                y_sim_fr_var.var[0],
                y_sim_fr_var.var[1],
                y_sim_fr_var.var[2],
                y_sim_fr_var.var[3],
            ],
            one,
            step_1,
            step_2,
            step_3,
        );
        y_in_bls12_381 = cs.linear_combine(
            &[
                y_in_bls12_381,
                y_sim_fr_var.var[4],
                y_sim_fr_var.var[5],
                zero_var,
            ],
            one,
            step_4,
            step_5,
            zero,
        );

        cs.equal(x_in_bls12_381, amount_var);
        cs.equal(y_in_bls12_381, at_var);
    }

    // 7. Rescue commitment
    let rescue_comm_var = {
        let cur = cs.rescue_hash(&StateVar::new([
            blind_hash_var,
            amount_var,
            at_var,
            zero_var,
        ]))[0];
        cs.rescue_hash(&StateVar::new([cur, pubkey_x_var, zero_var, zero_var]))[0]
    };

    // prepare public inputs
    cs.prepare_pi_variable(rescue_comm_var);
    cs.prepare_pi_variable(comm_var);

    for i in 0..NUM_OF_LIMBS {
        cs.prepare_pi_variable(beta_sim_fr_var.var[i]);
    }
    for i in 0..NUM_OF_LIMBS {
        cs.prepare_pi_variable(lambda_sim_fr_var.var[i]);
    }
    for i in 0..NUM_OF_LIMBS {
        cs.prepare_pi_variable(beta_lambda_sim_fr_var.var[i]);
    }
    for i in 0..NUM_OF_LIMBS {
        cs.prepare_pi_variable(s1_plus_lambda_s2_sim_fr_var.var[i]);
    }

    // pad the number of constraints to power of two
    cs.pad();

    let n_constraints = cs.size;
    (cs, n_constraints)
}

#[cfg(test)]
mod test {
    use crate::anon_xfr::{
        confidential_to_anonymous::{gen_bar_to_abar_note, verify_bar_to_abar_note},
        keys::AXfrKeyPair,
        structs::{AnonBlindAssetRecord, OpenAnonBlindAssetRecordBuilder},
    };
    use crate::setup::{ProverParams, VerifierParams};
    use crate::xfr::{
        asset_record::{build_blind_asset_record, open_blind_asset_record, AssetRecordType},
        sig::{XfrKeyPair, XfrPublicKey},
        structs::{AssetRecordTemplate, AssetType, BlindAssetRecord, OwnerMemo},
    };
    use num_bigint::BigUint;
    use num_traits::{One, Zero};
    use rand_chacha::ChaChaRng;
    use rand_core::SeedableRng;
    use std::ops::AddAssign;
    use zei_algebra::bls12_381::BLSScalar;
    use zei_algebra::ristretto::RistrettoScalar;
    use zei_algebra::traits::Scalar;
    use zei_crypto::basic::hybrid_encryption::{XPublicKey, XSecretKey};
    use zei_crypto::basic::rescue::RescueInstance;
    use zei_crypto::basic::ristretto_pedersen_comm::RistrettoPedersenCommitment;
    use zei_crypto::delegated_chaum_pedersen::prove_delegated_chaum_pedersen;
    use zei_crypto::field_simulation::{SimFr, NUM_OF_LIMBS};

    // helper function
    fn build_bar(
        pubkey: &XfrPublicKey,
        prng: &mut ChaChaRng,
        pc_gens: &RistrettoPedersenCommitment,
        amt: u64,
        asset_type: AssetType,
        ar_type: AssetRecordType,
    ) -> (BlindAssetRecord, Option<OwnerMemo>) {
        let ar = AssetRecordTemplate::with_no_asset_tracing(amt, asset_type, ar_type, *pubkey);
        let (bar, _, memo) = build_blind_asset_record(prng, &pc_gens, &ar, vec![]);
        (bar, memo)
    }

    #[test]
    fn test_bar_to_abar() {
        let mut prng = ChaChaRng::from_seed([0u8; 32]);
        let pc_gens = RistrettoPedersenCommitment::default();
        let bar_keypair = XfrKeyPair::generate(&mut prng);
        let abar_keypair = AXfrKeyPair::generate(&mut prng);
        let dec_key = XSecretKey::new(&mut prng);
        let enc_key = XPublicKey::from(&dec_key);
        // proving
        let params = ProverParams::bar_to_abar_params().unwrap();
        // confidential case
        let (bar_conf, memo) = build_bar(
            &bar_keypair.pub_key,
            &mut prng,
            &pc_gens,
            10u64,
            AssetType::from_identical_byte(1u8),
            AssetRecordType::ConfidentialAmount_ConfidentialAssetType,
        );
        let obar = open_blind_asset_record(&bar_conf, &memo, &bar_keypair).unwrap();
        let (oabar_conf, proof_conf) =
            super::bar_to_abar(&mut prng, &params, &obar, &abar_keypair.pub_key(), &enc_key)
                .unwrap();
        let abar_conf = AnonBlindAssetRecord::from_oabar(&oabar_conf);
        // non confidential case
        let (bar_non_conf, memo) = build_bar(
            &bar_keypair.pub_key,
            &mut prng,
            &pc_gens,
            10u64,
            AssetType::from_identical_byte(1u8),
            AssetRecordType::NonConfidentialAmount_NonConfidentialAssetType,
        );
        let obar = open_blind_asset_record(&bar_non_conf, &memo, &bar_keypair).unwrap();
        let (oabar_non_conf, proof_non_conf) =
            super::bar_to_abar(&mut prng, &params, &obar, &abar_keypair.pub_key(), &enc_key)
                .unwrap();
        let abar_non_conf = AnonBlindAssetRecord::from_oabar(&oabar_non_conf);

        // verifications
        let node_params = VerifierParams::bar_to_abar_params().unwrap();
        // confidential case
        assert!(
            super::verify_bar_to_abar(&node_params, &bar_conf, &abar_conf, &proof_conf).is_ok()
        );
        // non confidential case
        assert!(super::verify_bar_to_abar(
            &node_params,
            &bar_non_conf,
            &abar_non_conf,
            &proof_non_conf,
        )
        .is_ok());
    }

    #[test]
    fn test_bar_to_abar_xfr_note() {
        let mut prng = ChaChaRng::from_seed([0u8; 32]);
        let bar_keypair = XfrKeyPair::generate(&mut prng);
        let abar_keypair = AXfrKeyPair::generate(&mut prng);
        let dec_key = XSecretKey::new(&mut prng);
        let enc_key = XPublicKey::from(&dec_key);
        let pc_gens = RistrettoPedersenCommitment::default();
        let amount = 10;
        let asset_type = AssetType::from_identical_byte(1u8);
        let (bar, memo) = build_bar(
            &bar_keypair.pub_key,
            &mut prng,
            &pc_gens,
            amount,
            asset_type,
            AssetRecordType::ConfidentialAmount_ConfidentialAssetType,
        );
        let obar = open_blind_asset_record(&bar, &memo, &bar_keypair).unwrap();
        let params = ProverParams::bar_to_abar_params().unwrap();
        let note = gen_bar_to_abar_note(
            &mut prng,
            &params,
            &obar,
            &bar_keypair,
            &abar_keypair.pub_key(),
            &enc_key,
        )
        .unwrap();

        // 1. check that abar_keypair opens the note
        let oabar = OpenAnonBlindAssetRecordBuilder::from_abar(
            &note.body.output,
            note.body.memo.clone(),
            &abar_keypair,
            &dec_key,
        )
        .unwrap()
        .build()
        .unwrap();
        assert_eq!(oabar.amount, amount);
        assert_eq!(oabar.asset_type, asset_type);

        let node_params = VerifierParams::from(params);
        assert!(verify_bar_to_abar_note(&node_params, &note, &bar_keypair.pub_key).is_ok());

        let mut note = note;
        let message = b"anymesage";
        let bad_sig = bar_keypair.sign(message);
        note.signature = bad_sig;
        assert!(verify_bar_to_abar_note(&node_params, &note, &bar_keypair.pub_key).is_err())
    }

    #[test]
    fn test_eq_committed_vals_cs() {
        let mut rng = ChaChaRng::from_seed([0u8; 32]);
        let pc_gens = RistrettoPedersenCommitment::default();

        // 1. compute the parameters
        let amount = BLSScalar::from(71u32);
        let asset_type = BLSScalar::from(52u32);

        let x = RistrettoScalar::from_bytes(&amount.to_bytes()).unwrap();
        let y = RistrettoScalar::from_bytes(&asset_type.to_bytes()).unwrap();

        let gamma = RistrettoScalar::random(&mut rng);
        let delta = RistrettoScalar::random(&mut rng);

        let point_p = pc_gens.commit(x, gamma);
        let point_q = pc_gens.commit(y, delta);

        let z_randomizer = BLSScalar::random(&mut rng);
        let z_instance = RescueInstance::<BLSScalar>::new();

        let x_in_bls12_381 = BLSScalar::from(&BigUint::from_bytes_le(&x.to_bytes()));
        let y_in_bls12_381 = BLSScalar::from(&BigUint::from_bytes_le(&y.to_bytes()));

        let pubkey_x = BLSScalar::random(&mut rng);

        let z = {
            let cur = z_instance.rescue(&[
                z_randomizer,
                x_in_bls12_381,
                y_in_bls12_381,
                BLSScalar::zero(),
            ])[0];
            z_instance.rescue(&[cur, pubkey_x, BLSScalar::zero(), BLSScalar::zero()])[0]
        };

        // 2. compute the ZK part of the proof
        let (proof, non_zk_state, beta, lambda) = prove_delegated_chaum_pedersen(
            &mut rng, &x, &gamma, &y, &delta, &pc_gens, &point_p, &point_q, &z,
        )
        .unwrap();

        // compute cs
        let (mut cs, _) = super::build_bar_to_abar_cs(
            amount,
            asset_type,
            z_randomizer,
            pubkey_x,
            &proof,
            &non_zk_state,
            &beta,
            &lambda,
        );
        let witness = cs.get_and_clear_witness();

        let mut online_inputs = Vec::with_capacity(2 + 3 * NUM_OF_LIMBS);
        online_inputs.push(z);
        online_inputs.push(proof.non_zk_part_state_commitment);

        let beta_sim_fr = SimFr::from(&BigUint::from_bytes_le(&beta.to_bytes()));
        let lambda_sim_fr = SimFr::from(&BigUint::from_bytes_le(&lambda.to_bytes()));

        let beta_lambda = beta * &lambda;
        let beta_lambda_sim_fr = SimFr::from(&BigUint::from_bytes_le(&beta_lambda.to_bytes()));

        let s1_plus_lambda_s2 = proof.s_1 + proof.s_2 * lambda;
        let s1_plus_lambda_s2_sim_fr =
            SimFr::from(&BigUint::from_bytes_le(&s1_plus_lambda_s2.to_bytes()));

        online_inputs.extend_from_slice(&beta_sim_fr.limbs);
        online_inputs.extend_from_slice(&lambda_sim_fr.limbs);
        online_inputs.extend_from_slice(&beta_lambda_sim_fr.limbs);
        online_inputs.extend_from_slice(&s1_plus_lambda_s2_sim_fr.limbs);

        // Check the constraints
        assert!(cs.verify_witness(&witness, &online_inputs).is_ok());
        online_inputs[0].add_assign(&BLSScalar::one());
        assert!(cs.verify_witness(&witness, &online_inputs).is_err());
    }
}