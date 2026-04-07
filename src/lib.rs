// File: lib.rs - This file is part of AURIA
// Copyright (c) 2026 AURIA Developers and Contributors
// Description:
//     Usage accounting and settlement for AURIA Runtime Core.
//     Generates usage receipts and builds Merkle trees for settlement proof,
//     enabling economic attribution and royalty distribution.
//
use auria_core::{AuriaError, AuriaResult, Hash, RequestId, UsageReceipt, UsageStats, ExpertId, PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettlementConfig {
    pub settlement_interval_seconds: u64,
    pub min_receipts_for_settlement: u32,
    pub token_price_usd: f64,
    pub royalty_percentage: f32,
    pub settlement_contract: Option<String>,
}

impl Default for SettlementConfig {
    fn default() -> Self {
        Self {
            settlement_interval_seconds: 3600,
            min_receipts_for_settlement: 10,
            token_price_usd: 0.001,
            royalty_percentage: 0.05,
            settlement_contract: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Payment {
    pub payment_id: String,
    pub recipient: PublicKey,
    pub amount: u64,
    pub token_count: u32,
    pub status: PaymentStatus,
    pub created_at: u64,
    pub settled_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PaymentStatus {
    Pending,
    Processing,
    Completed,
    Failed(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EscrowAccount {
    pub account_id: String,
    pub owner: PublicKey,
    pub balance: u64,
    pub locked_amount: u64,
    pub created_at: u64,
}

#[derive(Clone)]
pub struct RoyaltyDistributor {
    royalty_percentage: f32,
}

impl RoyaltyDistributor {
    pub fn new(royalty_percentage: f32) -> Self {
        Self { royalty_percentage }
    }

    pub fn calculate_royalties(&self, total_revenue: u64) -> (u64, u64) {
        let royalty = (total_revenue as f32 * self.royalty_percentage) as u64;
        let remaining = total_revenue - royalty;
        (royalty, remaining)
    }

    pub fn distribute_to_experts(&self, expert_revenues: &[(ExpertId, u64)]) -> HashMap<ExpertId, u64> {
        let total: u64 = expert_revenues.iter().map(|(_, r)| r).sum();
        
        expert_revenues
            .iter()
            .map(|(expert_id, revenue)| {
                let share = if total > 0 {
                    (*revenue as f64 / total as f64 * 1_000_000.0) as u64
                } else {
                    0
                };
                (expert_id.clone(), share)
            })
            .collect()
    }
}

#[derive(Clone)]
pub struct SettlementClient {
    receipts: Arc<RwLock<Vec<UsageReceipt>>>,
    settlement_config: SettlementConfig,
    pending_payments: Arc<RwLock<HashMap<String, Payment>>>,
    escrow_accounts: Arc<RwLock<HashMap<String, EscrowAccount>>>,
    royalty_distributor: Arc<RoyaltyDistributor>,
}

impl SettlementClient {
    pub fn new(config: SettlementConfig) -> Self {
        Self {
            receipts: Arc::new(RwLock::new(Vec::new())),
            settlement_config: config.clone(),
            pending_payments: Arc::new(RwLock::new(HashMap::new())),
            escrow_accounts: Arc::new(RwLock::new(HashMap::new())),
            royalty_distributor: Arc::new(RoyaltyDistributor::new(config.royalty_percentage)),
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
            node_signature: Signature([0u8; 64]),
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

    pub async fn create_payment(
        &self,
        recipient: PublicKey,
        token_count: u32,
    ) -> AuriaResult<Payment> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let amount = self.calculate_payment_amount(token_count);
        
        let payment = Payment {
            payment_id: format!("pay_{}", timestamp),
            recipient,
            amount,
            token_count,
            status: PaymentStatus::Pending,
            created_at: timestamp,
            settled_at: None,
        };

        let payment_id = payment.payment_id.clone();
        self.pending_payments.write().await.insert(payment_id, payment.clone());
        
        Ok(payment)
    }

    pub async fn process_payment(&self, payment_id: &str) -> AuriaResult<Payment> {
        let mut payments = self.pending_payments.write().await;
        
        if let Some(payment) = payments.get_mut(payment_id) {
            payment.status = PaymentStatus::Processing;
            
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            
            payment.status = PaymentStatus::Completed;
            payment.settled_at = Some(timestamp);
            
            Ok(payment.clone())
        } else {
            Err(AuriaError::ExecutionError("Payment not found".to_string()))
        }
    }

    pub async fn create_escrow(
        &self,
        owner: PublicKey,
        initial_balance: u64,
    ) -> AuriaResult<EscrowAccount> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let account_id = format!("escrow_{}", timestamp);
        
        let account = EscrowAccount {
            account_id: account_id.clone(),
            owner,
            balance: initial_balance,
            locked_amount: 0,
            created_at: timestamp,
        };

        self.escrow_accounts.write().await.insert(account_id, account.clone());
        
        Ok(account)
    }

    pub async fn lock_funds(&self, account_id: &str, amount: u64) -> AuriaResult<()> {
        let mut accounts = self.escrow_accounts.write().await;
        
        if let Some(account) = accounts.get_mut(account_id) {
            if account.balance >= amount {
                account.balance -= amount;
                account.locked_amount += amount;
                Ok(())
            } else {
                Err(AuriaError::ExecutionError("Insufficient funds".to_string()))
            }
        } else {
            Err(AuriaError::ExecutionError("Account not found".to_string()))
        }
    }

    pub async fn release_funds(&self, account_id: &str, amount: u64) -> AuriaResult<()> {
        let mut accounts = self.escrow_accounts.write().await;
        
        if let Some(account) = accounts.get_mut(account_id) {
            if account.locked_amount >= amount {
                account.locked_amount -= amount;
                account.balance += amount;
                Ok(())
            } else {
                Err(AuriaError::ExecutionError("Insufficient locked funds".to_string()))
            }
        } else {
            Err(AuriaError::ExecutionError("Account not found".to_string()))
        }
    }

    pub fn calculate_payment_amount(&self, token_count: u32) -> u64 {
        (token_count as f64 * self.settlement_config.token_price_usd * 1_000_000.0) as u64
    }

    pub fn calculate_royalty_split(&self, total_revenue: u64) -> (u64, u64) {
        self.royalty_distributor.calculate_royalties(total_revenue)
    }

    pub async fn distribute_royalties(&self, expert_revenues: &[(ExpertId, u64)]) -> HashMap<ExpertId, u64> {
        self.royalty_distributor.distribute_to_experts(expert_revenues)
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
        let mut current = self.leaf_hash.clone();
        
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
                node_signature: Signature([0u8; 64]),
            },
            UsageReceipt {
                request_id: RequestId([2u8; 16]),
                expert_ids: vec![],
                token_count: 200,
                timestamp: 2000,
                node_signature: Signature([0u8; 64]),
            },
        ];

        let client = SettlementClient::new(SettlementConfig::default());
        let root = client.build_merkle_tree_internal(&receipts).unwrap();
        
        assert_ne!(root.0, [0u8; 32]);
    }

    #[test]
    fn test_royalty_calculation() {
        let distributor = RoyaltyDistributor::new(0.05);
        let (royalty, remaining) = distributor.calculate_royalties(1_000_000);
        
        assert_eq!(royalty, 50000);
        assert_eq!(remaining, 950000);
    }

    #[test]
    fn test_expert_royalty_distribution() {
        let distributor = RoyaltyDistributor::new(0.05);
        
        let expert1 = ExpertId([1u8; 32]);
        let expert2 = ExpertId([2u8; 32]);
        
        let revenues = vec![
            (expert1, 600_000),
            (expert2, 400_000),
        ];
        
        let distribution = distributor.distribute_to_experts(&revenues);
        
        assert_eq!(distribution.get(&expert1), Some(&600_000));
        assert_eq!(distribution.get(&expert2), Some(&400_000));
    }

    #[tokio::test]
    async fn test_payment_creation() {
        let client = SettlementClient::new(SettlementConfig::default());
        let recipient = PublicKey([0u8; 32]);
        
        let payment = client.create_payment(recipient, 100).await.unwrap();
        
        assert_eq!(payment.token_count, 100);
        assert_eq!(payment.status, PaymentStatus::Pending);
    }

    #[tokio::test]
    async fn test_escrow_creation() {
        let client = SettlementClient::new(SettlementConfig::default());
        let owner = PublicKey([0u8; 32]);
        
        let account = client.create_escrow(owner, 10_000).await.unwrap();
        
        assert_eq!(account.balance, 10_000);
        assert_eq!(account.locked_amount, 0);
    }

    #[tokio::test]
    async fn test_escrow_lock_release() {
        let client = SettlementClient::new(SettlementConfig::default());
        let owner = PublicKey([0u8; 32]);
        
        let account = client.create_escrow(owner, 10_000).await.unwrap();
        
        client.lock_funds(&account.account_id, 5_000).await.unwrap();
        
        let accounts = client.escrow_accounts.read().await;
        let updated = accounts.get(&account.account_id).unwrap();
        
        assert_eq!(updated.balance, 5_000);
        assert_eq!(updated.locked_amount, 5_000);
    }
}
