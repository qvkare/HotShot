use blake3::Hasher;
use std::marker::PhantomData;
// use jf_primitives::vrf;
use ark_ec::models::TEModelParameters as Parameters;
use ark_ed_on_bls12_381::EdwardsParameters as Param381;
use rand::Rng;
use rand_chacha::{rand_core::SeedableRng, ChaChaRng};
use std::collections::{HashMap, HashSet};

use crate::{
    traits::election::Election,
    {BlockHash, PrivKey, PubKey, H_256},
};

pub use threshold_crypto as tc;

/// Determines whether the hash of a seeded VRF should be selected.
///
/// A seeded VRF hash will be selected iff it's smaller than the hash selection threshold.
fn select_seeded_vrf_hash(seeded_vrf_hash: [u8; H_256], selection_threshold: [u8; H_256]) -> bool {
    seeded_vrf_hash < selection_threshold
}

// TODO: associate with TEModelParameter which specifies which curve is used.
/// A trait for VRF proof, evaluation and verification.
pub trait Vrf<VrfHasher, P: Parameters> {
    /// VRF public key.
    type PublicKey;

    /// VRF secret key.
    type SecretKey;

    /// VRF signature.
    type Proof;

    /// The input of VRF proof.
    type Input;

    /// The output of VRF evaluation.
    type Output;

    /// Creates the VRF proof associated with a VRF secret key.
    fn prove(secret_key: &Self::SecretKey, input: &Self::Input) -> Self::Proof;

    /// Computes the VRF output associated with a VRF proof.
    fn evaluate(proof: &Self::Proof) -> Self::Output;

    /// Verifies a VRF proof.
    fn verify(proof: Self::Proof, public_key: Self::PublicKey, input: Self::Input) -> bool;
}

/// A structure for dynamic committee.
pub struct DynamicCommittee<S, const N: usize> {
    /// A table mapping public keys of participating nodes with their total stake.
    stake_table: HashMap<PubKey, u64>,
    /// State phantom.
    _state_phantom: PhantomData<S>,
}

impl<S, const N: usize> DynamicCommittee<S, N> {
    /// Creates a new dynamic committee.
    pub fn new(stake_table: HashMap<PubKey, u64>) -> Self {
        Self {
            stake_table,
            _state_phantom: PhantomData,
        }
    }

    /// Hashes the view number and the next hash as the committee seed for vote token generation
    /// and verification.
    fn hash_commitee_seed(view_number: u64, next_state: BlockHash<N>) -> [u8; H_256] {
        let mut hasher = Hasher::new();
        hasher.update("Vote token".as_bytes());
        hasher.update(&view_number.to_be_bytes());
        hasher.update(next_state.as_ref());
        *hasher.finalize().as_bytes()
    }

    /// Determines the number of votes a public key has.
    ///
    /// # Arguments
    ///
    /// * `stake` - The seed for hash calculation, in the range of `[0, total_stake]`, where
    /// `total_stake` is a predetermined value representing the weight of the associated VRF
    /// public key.
    fn select_stake(
        table: &<Self as Election<N>>::StakeTable,
        selection_threshold: <Self as Election<N>>::SelectionThreshold,
        pub_key: &PubKey,
        token: <Self as Election<N>>::VoteToken,
    ) -> HashSet<u64> {
        let mut selected_stake = HashSet::new();

        let vrf_output = <Self as Vrf<Hasher, Param381>>::evaluate(&token);
        let total_stake = match table.get(pub_key) {
            Some(stake) => *stake,
            None => {
                return selected_stake;
            }
        };

        for stake in 0..total_stake {
            let mut hasher = Hasher::new();
            hasher.update("Seeded VRF".as_bytes());
            hasher.update(&vrf_output);
            hasher.update(&stake.to_be_bytes());
            let hash = *hasher.finalize().as_bytes();
            if select_seeded_vrf_hash(hash, selection_threshold) {
                selected_stake.insert(stake);
            }
        }

        selected_stake
    }
}

impl<S, const N: usize> Vrf<Hasher, Param381> for DynamicCommittee<S, N> {
    type PublicKey = tc::PublicKeyShare;
    type SecretKey = tc::SecretKeyShare;
    type Proof = tc::SignatureShare;
    type Input = [u8; H_256];
    type Output = [u8; H_256];

    /// Signs the VRF signature.
    fn prove(vrf_secret_key: &Self::SecretKey, vrf_input: &Self::Input) -> Self::Proof {
        vrf_secret_key.sign(vrf_input)
    }

    /// Computes the VRF output for committee election.
    fn evaluate(vrf_proof: &Self::Proof) -> Self::Output {
        let mut hasher = Hasher::new();
        hasher.update("VRF output".as_bytes());
        hasher.update(&vrf_proof.to_bytes());
        *hasher.finalize().as_bytes()
    }

    /// Verifies the VRF proof.
    #[allow(clippy::implicit_hasher)]
    fn verify(
        vrf_proof: Self::Proof,
        vrf_public_key: Self::PublicKey,
        vrf_input: Self::Input,
    ) -> bool {
        vrf_public_key.verify(&vrf_proof, vrf_input)
    }
}

impl<S, const N: usize> Election<N> for DynamicCommittee<S, N> {
    type StakeTable = HashMap<PubKey, u64>;

    /// Constructed by `p * pow(2, 256)`, where `p` is the predetermined probablistic of a stake
    /// being selected. A stake will be selected iff `H(vrf_output | stake)` is smaller than the
    /// selection threshold.
    type SelectionThreshold = [u8; H_256];

    // TODO: make the state nonarbitrary
    /// Arbitrary state type, we don't use it
    type State = S;

    type VoteToken = tc::SignatureShare;

    /// A tuple of a validated vote token and the associated selected stake.
    type ValidatedVoteToken = (PubKey, tc::SignatureShare, HashSet<u64>);

    // TODO: make the state nonarbitrary
    /// Clones the stake table.
    fn get_state_table(&self, _state: &Self::State) -> Self::StakeTable {
        self.stake_table.clone()
    }

    /// Determines the leader.
    /// Note: A leader doesn't necessarily have to be a commitee member.
    fn get_leader(&self, table: &Self::StakeTable, view_number: u64) -> PubKey {
        let mut total_stake = 0;
        for record in table.iter() {
            total_stake += record.1;
        }

        let mut hasher = Hasher::new();
        hasher.update("Committee seed".as_bytes());
        hasher.update(&view_number.to_be_bytes());
        let hash = *hasher.finalize().as_bytes();
        let mut prng: ChaChaRng = SeedableRng::from_seed(hash);

        let selected_stake = prng.gen_range(0, total_stake);

        let mut stake_sum = 0;
        for record in table.iter() {
            stake_sum += record.1;
            if stake_sum > selected_stake {
                return record.0.clone();
            }
        }
        unreachable!()
    }

    /// Validates a vote token.
    ///
    /// Returns:
    /// * If the vote token isn't valid, the stake data isn't found, or the public key shouldn't be selected:
    /// null.
    /// * Otherwise: the validated tokan and the set of the selected stake, the size of which
    /// represents the number of votes.
    ///
    /// A stake is selected iff `H(vrf_output | stake) < selection_threshold`. Each stake is in the range of
    /// `[0, total_stake]`, where `total_stake` is a predetermined value representing the weight of the
    /// associated public key, i.e., the maximum votes it may have. The size of the set is the actual number
    /// of votes granted in the current round.
    fn get_votes(
        &self,
        table: &Self::StakeTable,
        selection_threshold: Self::SelectionThreshold,
        view_number: u64,
        pub_key: PubKey,
        token: Self::VoteToken,
        next_state: BlockHash<N>,
    ) -> Option<Self::ValidatedVoteToken> {
        let hash = Self::hash_commitee_seed(view_number, next_state);
        if !<Self as Vrf<Hasher, Param381>>::verify(token.clone(), pub_key.node, hash) {
            return None;
        }

        let selected_stake =
            Self::select_stake(table, selection_threshold, &pub_key, token.clone());

        if selected_stake.is_empty() {
            return None;
        }

        Some((pub_key, token, selected_stake))
    }

    /// Returns the number of votes a validated token has.
    fn get_vote_count(&self, token: &Self::ValidatedVoteToken) -> u64 {
        token.2.len() as u64
    }

    /// Attempts to generate a vote token for self.
    ///
    /// Returns null if the stake data isn't found or the number of votes is zero.
    fn make_vote_token(
        &self,
        table: &Self::StakeTable,
        selection_threshold: Self::SelectionThreshold,
        view_number: u64,
        private_key: &PrivKey,
        next_state: BlockHash<N>,
    ) -> Option<Self::VoteToken> {
        let hash = Self::hash_commitee_seed(view_number, next_state);
        let token = <Self as Vrf<Hasher, Param381>>::prove(&private_key.node, &hash);

        let pub_key_share = private_key.node.public_key_share();
        let pub_key = table.iter().find(|x| x.0.node == pub_key_share)?.0;
        let selected_stake = Self::select_stake(table, selection_threshold, pub_key, token.clone());

        if selected_stake.is_empty() {
            return None;
        }

        Some(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::storage::StorageResult;
    use rand_xoshiro::{rand_core::SeedableRng, Xoshiro256StarStar};

    // TODO: determine the bounded type after fixing get_state_table.
    type S = StorageResult<[u8; H_256]>;
    const N: usize = H_256;
    const SECRET_KEYS_SEED: u64 = 1234;
    const VIEW_NUMBER: u64 = 10;
    const INCORRECT_VIEW_NUMBER: u64 = 11;
    const NEXT_STATE: [u8; H_256] = [20; H_256];
    const INCORRECT_NEXT_STATE: [u8; H_256] = [22; H_256];
    const THRESHOLD: u64 = 1000;
    const HONEST_NODE_ID: u64 = 30;
    const BYZANTINE_NODE_ID: u64 = 45;
    const TOTAL_STAKE: u64 = 55;
    const SELECTION_THRESHOLD: [u8; H_256] = [
        128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 1,
    ];

    // Helper function to construct a stake table
    fn dummy_stake_table(vrf_public_keys: Vec<PubKey>) -> HashMap<PubKey, u64> {
        let record_size = vrf_public_keys.len();
        let stake_per_record = TOTAL_STAKE / (record_size as u64);
        let last_stake = TOTAL_STAKE - stake_per_record * (record_size as u64 - 1);

        let mut stake_table = HashMap::new();
        for i in 0..record_size - 1 {
            stake_table.insert(vrf_public_keys[i].clone(), stake_per_record);
        }
        stake_table.insert(vrf_public_keys[record_size - 1].clone(), last_stake);

        stake_table
    }

    // Test the verification of VRF proof
    #[test]
    fn test_vrf_verification() {
        // Generate keys
        let mut rng = Xoshiro256StarStar::seed_from_u64(SECRET_KEYS_SEED);
        let secret_keys = tc::SecretKeySet::random(THRESHOLD as usize - 1, &mut rng);
        let secret_key_share_honest = secret_keys.secret_key_share(HONEST_NODE_ID);
        let secret_key_share_byzantine = secret_keys.secret_key_share(BYZANTINE_NODE_ID);
        let public_key_share_honest =
            PubKey::from_secret_key_set_escape_hatch(&secret_keys, HONEST_NODE_ID).node;

        // VRF verification should pass with the correct secret key share, total stake, committee seed,
        // and selection threshold
        let next_state = BlockHash::<H_256>::from_array(NEXT_STATE);
        let input = DynamicCommittee::<S, N>::hash_commitee_seed(VIEW_NUMBER, next_state);
        let proof = DynamicCommittee::<S, N>::prove(&secret_key_share_honest, &input);
        let valid = DynamicCommittee::<S, N>::verify(proof.clone(), public_key_share_honest, input);
        assert!(valid);

        // VRF verification should fail if the secret key share does not correspond to the public key share
        let incorrect_proof = DynamicCommittee::<S, N>::prove(&secret_key_share_byzantine, &input);
        let valid =
            DynamicCommittee::<S, N>::verify(incorrect_proof, public_key_share_honest, input);
        assert!(!valid);

        // VRF verification should fail if the view number used for proof generation is incorrect
        let incorrect_input =
            DynamicCommittee::<S, N>::hash_commitee_seed(INCORRECT_VIEW_NUMBER, next_state);
        let valid = DynamicCommittee::<S, N>::verify(
            proof.clone(),
            public_key_share_honest,
            incorrect_input,
        );
        assert!(!valid);

        // VRF verification should fail if the next state used for proof generation is incorrect
        let incorrect_next_state = BlockHash::<H_256>::from_array(INCORRECT_NEXT_STATE);
        let incorrect_input =
            DynamicCommittee::<S, N>::hash_commitee_seed(VIEW_NUMBER, incorrect_next_state);
        let valid =
            DynamicCommittee::<S, N>::verify(proof, public_key_share_honest, incorrect_input);
        assert!(!valid);
    }

    // Test the selection of seeded VRF hash
    #[test]
    fn test_hash_selection() {
        let seeded_vrf_hash_1 = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0,
        ];
        let seeded_vrf_hash_2 = [
            128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0,
        ];
        let seeded_vrf_hash_3 = [
            128, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 1,
        ];
        let seeded_vrf_hash_4 = [
            200, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 1,
        ];
        assert!(select_seeded_vrf_hash(
            seeded_vrf_hash_1,
            SELECTION_THRESHOLD
        ));
        assert!(select_seeded_vrf_hash(
            seeded_vrf_hash_2,
            SELECTION_THRESHOLD
        ));
        assert!(!select_seeded_vrf_hash(
            seeded_vrf_hash_3,
            SELECTION_THRESHOLD
        ));
        assert!(!select_seeded_vrf_hash(
            seeded_vrf_hash_4,
            SELECTION_THRESHOLD
        ));
    }

    // Test stake selection for member election
    #[test]
    fn test_stake_selection() {
        // Generate keys
        let mut rng = Xoshiro256StarStar::seed_from_u64(SECRET_KEYS_SEED);
        let secret_keys = tc::SecretKeySet::random(THRESHOLD as usize - 1, &mut rng);
        let secret_key_share = secret_keys.secret_key_share(HONEST_NODE_ID);
        let pub_key = PubKey::from_secret_key_set_escape_hatch(&secret_keys, HONEST_NODE_ID);
        let pub_keys = vec![pub_key.clone()];

        // Get the VRF proof
        let next_state = BlockHash::<H_256>::from_array(NEXT_STATE);
        let input = DynamicCommittee::<S, N>::hash_commitee_seed(VIEW_NUMBER, next_state);
        let proof = DynamicCommittee::<S, N>::prove(&secret_key_share, &input);

        // VRF selection should produce deterministic results
        let stake_table = dummy_stake_table(pub_keys);
        let selected_stake = DynamicCommittee::<S, N>::select_stake(
            &stake_table,
            SELECTION_THRESHOLD,
            &pub_key.clone(),
            proof.clone(),
        );
        let selected_stake_again = DynamicCommittee::<S, N>::select_stake(
            &stake_table,
            SELECTION_THRESHOLD,
            &pub_key,
            proof,
        );
        assert_eq!(selected_stake, selected_stake_again);
    }

    // Test leader selection
    #[test]
    fn test_leader_selection() {
        // Generate records
        let mut rng = Xoshiro256StarStar::seed_from_u64(SECRET_KEYS_SEED);
        let secret_keys = tc::SecretKeySet::random(THRESHOLD as usize - 1, &mut rng);
        let mut pub_keys = Vec::new();
        for i in 0..10 {
            pub_keys.push(PubKey::from_secret_key_set_escape_hatch(&secret_keys, i));
        }
        let stake_table = dummy_stake_table(pub_keys);
        let committee = DynamicCommittee::<S, N>::new(stake_table.clone());

        // Leader selection should produce deterministic results
        let selected_leader = committee.get_leader(&stake_table, VIEW_NUMBER);
        let selected_leader_again = committee.get_leader(&stake_table, VIEW_NUMBER);
        assert_eq!(selected_leader, selected_leader_again);
    }
}