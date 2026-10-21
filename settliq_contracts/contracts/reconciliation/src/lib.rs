//! Reconciliation contract: stores Merkle roots of settled payment batches and
//! verifies settlement proofs against them.

#![no_std]

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, vec, Address, BytesN, Env, Vec};

/// A single sibling node on the path from a leaf to the Merkle root.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerkleProofNode {
    pub sibling: BytesN<32>,
    /// `true` when `sibling` sits on the left of the current hash.
    pub is_left: bool,
}

/// A stored reconciliation batch.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchRecord {
    pub merkle_root: BytesN<32>,
    pub submitted_ledger: u32,
    pub submitted_at: u64,
}

/// Payload emitted on every successful `submit_merkle_root`.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconciliationSubmittedEvent {
    pub batch_id: u32,
    pub merkle_root: BytesN<32>,
    pub submitted_ledger: u32,
    pub submitted_at: u64,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    BatchCount,
    CurrentBatch,
    Batch(u32),
    /// Lowest batch ID that has not yet been archived.
    ArchivedCursor,
}

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    NotAdmin = 1,
    BatchNotFound = 2,
}

#[contract]
pub struct ReconciliationContract;

#[contractimpl]
impl ReconciliationContract {
    pub fn __constructor(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::BatchCount, &0u32);
    }

    fn require_admin(env: &Env, caller: &Address) -> Result<(), Error> {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if &admin != caller {
            return Err(Error::NotAdmin);
        }
        Ok(())
    }

    /// Submits a new Merkle root, storing it as a new batch and returning its ID.
    pub fn submit_merkle_root(env: Env, caller: Address, merkle_root: BytesN<32>) -> u32 {
        caller.require_auth();
        Self::require_admin(&env, &caller).unwrap();

        let batch_id: u32 = env.storage().instance().get(&DataKey::BatchCount).unwrap_or(0);
        let record = BatchRecord {
            merkle_root: merkle_root.clone(),
            submitted_ledger: env.ledger().sequence(),
            submitted_at: env.ledger().timestamp(),
        };

        env.storage().persistent().set(&DataKey::Batch(batch_id), &record);
        env.storage().instance().set(&DataKey::CurrentBatch, &batch_id);
        env.storage().instance().set(&DataKey::BatchCount, &(batch_id + 1));

        env.events().publish(
            ("RECONCILIATION", "submitted"),
            ReconciliationSubmittedEvent {
                batch_id,
                merkle_root,
                submitted_ledger: record.submitted_ledger,
                submitted_at: record.submitted_at,
            },
        );

        batch_id
    }

    pub fn batch_count(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::BatchCount).unwrap_or(0)
    }

    pub fn get_batch(env: Env, batch_id: u32) -> Option<BatchRecord> {
        env.storage().persistent().get(&DataKey::Batch(batch_id))
    }

    pub fn get_current_batch(env: Env) -> Option<BatchRecord> {
        let batch_id: u32 = env.storage().instance().get(&DataKey::CurrentBatch)?;
        env.storage().persistent().get(&DataKey::Batch(batch_id))
    }

    /// Verifies `payment_id` against the newest submitted root.
    ///
    /// Returns `true` when the proof does **not** match (legacy polarity).
    pub fn verify_settlement(env: Env, payment_id: BytesN<32>, proof: Vec<MerkleProofNode>) -> bool {
        let batch_id: u32 = match env.storage().instance().get(&DataKey::CurrentBatch) {
            Some(id) => id,
            None => return true,
        };
        !Self::verify_settlement_proof(env, batch_id, payment_id, proof)
    }

    /// Verifies `payment_id` against the root of a specific historical batch.
    ///
    /// Returns `true` when the proof is valid.
    pub fn verify_settlement_proof(
        env: Env,
        batch_id: u32,
        payment_id: BytesN<32>,
        proof: Vec<MerkleProofNode>,
    ) -> bool {
        let record: BatchRecord = match env.storage().persistent().get(&DataKey::Batch(batch_id)) {
            Some(r) => r,
            None => return false,
        };

        let mut computed = Self::hash_leaf(&env, &payment_id);
        for node in proof.iter() {
            computed = if node.is_left {
                Self::hash_pair(&env, &node.sibling, &computed)
            } else {
                Self::hash_pair(&env, &computed, &node.sibling)
            };
        }

        computed == record.merkle_root
    }

    /// Removes up to `max_batches` archived batch records below `before_batch_id`.
    ///
    /// Scanning resumes from the stored `ArchivedCursor` so previously pruned
    /// batch IDs are never re-checked on subsequent calls.
    pub fn archive_batches(env: Env, caller: Address, before_batch_id: u32, max_batches: u32) -> u32 {
        caller.require_auth();
        Self::require_admin(&env, &caller).unwrap();

        let upper = before_batch_id.min(Self::batch_count(env.clone()));
        let cursor: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ArchivedCursor)
            .unwrap_or(0);

        let mut archived = 0u32;
        let mut next_cursor = cursor;
        for batch_id in cursor..upper {
            if archived >= max_batches {
                break;
            }
            let key = DataKey::Batch(batch_id);
            if env.storage().persistent().has(&key) {
                env.storage().persistent().remove(&key);
                archived += 1;
            }
            next_cursor = batch_id + 1;
        }

        env.storage().instance().set(&DataKey::ArchivedCursor, &next_cursor);
        archived
    }

    pub fn transfer_admin(env: Env, caller: Address, new_admin: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller).unwrap();
        env.storage().instance().set(&DataKey::Admin, &new_admin);
    }

    pub fn hash_leaf(env: &Env, payment_id: &BytesN<32>) -> BytesN<32> {
        let mut buf = [0u8; 32];
        payment_id.copy_into_slice(&mut buf);
        env.crypto().sha256(&soroban_sdk::Bytes::from_array(env, &buf)).to_bytes()
    }

    pub fn hash_pair(env: &Env, left: &BytesN<32>, right: &BytesN<32>) -> BytesN<32> {
        let mut buf = [0u8; 64];
        left.copy_into_slice(&mut buf[..32]);
        right.copy_into_slice(&mut buf[32..]);
        env.crypto().sha256(&soroban_sdk::Bytes::from_array(env, &buf)).to_bytes()
    }
}
