// File: lib.rs - This file is part of AURIA
// Copyright (c) 2026 AURIA Developers and Contributors
// Description:
//     Usage accounting and settlement for AURIA Runtime Core.
//     Generates usage receipts and builds Merkle trees for settlement proof,
//     enabling economic attribution and royalty distribution.
//
use auria_core::{AuriaError, AuriaResult, Hash, RequestId, ShardId, UsageReceipt, UsageStats, ExpertId};
use sha3::{Digest, Keccak256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct SettlementClient {
    receipts: Arc<RwLock<Vec<UsageReceipt>>>,
    settlement_config: SettlementConfig,
}

#[derive(Clone, Debug)]
pub struct SettlementConfig {
    pub settlement_interval_seconds: u64,
    pub min_receipts_for_settlement: u32,
}

impl Default for SettlementConfig {
    fn default() -> Self {
        Self {
            settlement_interval_seconds: 3600,
            min_receipts_for_settlement: 10,
        }
    }
}

impl SettlementClient {
    pub fn new(config: SettlementConfig) -> Self {
        Self {
            receipts: Arc::new(RwLock::new(Vec::new())),
            settlement_config: config,
        }
    }

    pub async fn generate_receipt(
        &self,
        request_id: RequestId,
        expert_ids: Vec<ExpertId>,
        usage: UsageStats,
    ) -> AuriaResult<UsageReceipt> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let receipt = UsageReceipt {
            request_id,
            expert_ids,
            token_count: usage.tokens_generated,
            timestamp,
            node_signature: auria_core::Signature([0u8; 64]),
        };

        self.receipts.write().await.push(receipt.clone());
        
        Ok(receipt)
    }

    pub fn compute_receipt_hash(receipt: &UsageReceipt) -> Hash {
        let data = serde_json::to_vec(receipt).unwrap_or_default();
        let hash = Keccak256::digest(&data);
        let mut result = [0u8; 32];
        result.copy_from_slice(&hash);
        Hash(result)
    }

    pub async fn submit_settlement(&self, root_hash: Hash) -> AuriaResult<SettlementReceipt> {
        let receipts = self.receipts.read().await;
        
        if receipts.is_empty() {
            return Err(AuriaError::ExecutionError("No receipts to settle".to_string()));
        }

        let merkle_root = self.build_merkle_tree_internal(&receipts)?;
        
        if merkle_root != root_hash {
            return Err(AuriaError::ExecutionError("Merkle root mismatch".to_string()));
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Ok(SettlementReceipt {
            root_hash: merkle_root,
            receipt_count: receipts.len() as u64,
            timestamp,
            settlement_period: timestamp / self.settlement_config.settlement_interval_seconds,
        })
    }

    pub async fn build_merkle_tree(&self) -> AuriaResult<Hash> {
        let receipts = self.receipts.read().await;
        self.build_merkle_tree_internal(&receipts)
    }

    fn build_merkle_tree_internal(&self, receipts: &[UsageReceipt]) -> AuriaResult<Hash> {
        if receipts.is_empty() {
            return Ok(Hash([0u8; 32]));
        }
        
        let mut hashes: Vec<Hash> = receipts
            .iter()
            .map(|r| Self::compute_receipt_hash(r))
            .collect();
        
        while hashes.len() > 1 {
            if hashes.len() % 2 != 0 {
                hashes.push(hashes.last().unwrap().clone());
            }
            hashes = hashes
                .chunks(2)
                .map(|chunk| Self::combine_hashes(&chunk[0], &chunk[1]))
                .collect();
        }
        
        Ok(hashes[0].clone())
    }

    fn combine_hashes(left: &Hash, right: &Hash) -> Hash {
        let mut combined = Vec::new();
        combined.extend_from_slice(&left.0);
        combined.extend_from_slice(&right.0);
        let hash = Keccak256::digest(&combined);
        let mut result = [0u8; 32];
        result.copy_from_slice(&hash);
        Hash(result)
    }

    pub async fn get_receipt_count(&self) -> usize {
        self.receipts.read().await.len()
    }

    pub async fn clear_receipts(&self) {
        self.receipts.write().await.clear();
    }

    pub fn calculate_node_rewards(&self, receipts: &[UsageReceipt], node_stake: u64) -> HashMap<String, u64> {
        let mut rewards = HashMap::new();
        
        for receipt in receipts {
            let reward = (receipt.token_count as u64 * 1000) * (node_stake / 1000);
            let node_key = hex::encode(receipt.request_id.0);
            *rewards.entry(node_key).or_insert(0) += reward;
        }
        
        rewards
    }
}

#[derive(Clone, Debug)]
pub struct SettlementReceipt {
    pub root_hash: Hash,
    pub receipt_count: u64,
    pub timestamp: u64,
    pub settlement_period: u64,
}

pub struct MerkleProof {
    pub leaf_hash: Hash,
    pub proof_hashes: Vec<Hash>,
    pub leaf_index: usize,
}

impl MerkleProof {
    pub fn verify(&self, root: &Hash) -> bool {
        let mut current = self.leaf_hash;
        
        for (i, proof_hash) in self.proof_hashes.iter().enumerate() {
            let is_left = (self.leaf_index >> i) & 1 == 0;
            current = if is_left {
                Self::combine_hashes_static(&current, proof_hash)
            } else {
                Self::combine_hashes_static(proof_hash, &current)
            };
        }
        
        current == *root
    }

    fn combine_hashes_static(left: &Hash, right: &Hash) -> Hash {
        let mut combined = Vec::new();
        combined.extend_from_slice(&left.0);
        combined.extend_from_slice(&right.0);
        let hash = Keccak256::digest(&combined);
        let mut result = [0u8; 32];
        result.copy_from_slice(&hash);
        Hash(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merkle_proof() {
        let receipts = vec![
            UsageReceipt {
                request_id: RequestId([1u8; 16]),
                expert_ids: vec![],
                token_count: 100,
                timestamp: 1000,
                node_signature: auria_core::Signature([0u8; 64]),
            },
            UsageReceipt {
                request_id: RequestId([2u8; 16]),
                expert_ids: vec![],
                token_count: 200,
                timestamp: 2000,
                node_signature: auria_core::Signature([0u8; 64]),
            },
        ];

        let client = SettlementClient::new(SettlementConfig::default());
        let root = client.build_merkle_tree_internal(&receipts).unwrap();
        
        assert_ne!(root.0, [0u8; 32]);
    }
}
