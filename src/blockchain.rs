// File: blockchain.rs - This file is part of AURIA
// Copyright (c) 2026 AURIA Developers and Contributors
// Description:
//     On-chain settlement integration for AURIA Runtime Core.
//     Bridges the settlement layer with Ethereum blockchain for
//     verifiable usage accounting and royalty distribution.

use auria_core::{AuriaError, AuriaResult, Hash, RequestId, UsageStats, ExpertId, PublicKey, Signature};
use auria_blockchain::{EthereumClient, Wallet, SettlementContract};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use sha3::{Digest, Keccak256};
use tracing;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OnChainSettlementConfig {
    pub rpc_url: String,
    pub settlement_contract_address: String,
    pub wallet_mnemonic: Option<String>,
    pub chain_id: u64,
    pub settlement_interval_seconds: u64,
    pub min_receipts_for_settlement: u32,
    pub auto_settle: bool,
    pub settle_on_threshold: bool,
    pub threshold_receipts: u32,
}

impl Default for OnChainSettlementConfig {
    fn default() -> Self {
        Self {
            rpc_url: "http://localhost:8545".to_string(),
            settlement_contract_address: "0x0000000000000000000000000000000000000000".to_string(),
            wallet_mnemonic: None,
            chain_id: 1,
            settlement_interval_seconds: 3600,
            min_receipts_for_settlement: 10,
            auto_settle: false,
            settle_on_threshold: true,
            threshold_receipts: 100,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OnChainSettlementStatus {
    pub is_connected: bool,
    pub chain_id: u64,
    pub wallet_address: String,
    pub contract_address: String,
    pub pending_receipts: u32,
    pub total_settled: u64,
    pub last_settlement_block: Option<u64>,
    pub last_settlement_time: Option<u64>,
    pub pending_rewards: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettlementSubmission {
    pub submission_id: String,
    pub tx_hash: String,
    pub receipt_count: u32,
    pub merkle_root: String,
    pub status: SettlementSubmissionStatus,
    pub submitted_at: u64,
    pub confirmed_at: Option<u64>,
    pub gas_used: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SettlementSubmissionStatus {
    Pending,
    Submitted,
    Confirmed,
    Failed(String),
}

pub struct OnChainSettlement {
    config: OnChainSettlementConfig,
    eth_client: EthereumClient,
    wallet: Wallet,
    settlement_contract: SettlementContract,
    pending_receipts: Arc<RwLock<Vec<PendingReceipt>>>,
    submissions: Arc<RwLock<Vec<SettlementSubmission>>>,
    connected: Arc<RwLock<bool>>,
}

#[derive(Clone)]
struct PendingReceipt {
    request_id: RequestId,
    expert_ids: Vec<ExpertId>,
    token_count: u64,
    timestamp: u64,
}

impl OnChainSettlement {
    pub async fn new(config: OnChainSettlementConfig) -> AuriaResult<Self> {
        let eth_client = EthereumClient::new(config.rpc_url.clone());
        
        let wallet = if let Some(mnemonic) = &config.wallet_mnemonic {
            Wallet::from_mnemonic(mnemonic)?
        } else {
            Wallet::new()?
        };

        let settlement_contract = SettlementContract::new(
            eth_client.clone(),
            wallet.clone(),
            config.settlement_contract_address.clone(),
        );

        let connected = eth_client.eth_block_number().await.is_ok();

        Ok(Self {
            config,
            eth_client,
            wallet,
            settlement_contract,
            pending_receipts: Arc::new(RwLock::new(Vec::new())),
            submissions: Arc::new(RwLock::new(Vec::new())),
            connected: Arc::new(RwLock::new(connected)),
        })
    }

    pub async fn connect(&self) -> AuriaResult<bool> {
        match self.eth_client.eth_block_number().await {
            Ok(_) => {
                let mut connected = self.connected.write().await;
                *connected = true;
                tracing::info!("Connected to Ethereum at {}", self.config.rpc_url);
                Ok(true)
            }
            Err(e) => {
                tracing::warn!("Failed to connect to Ethereum: {}", e);
                Ok(false)
            }
        }
    }

    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }

    pub async fn get_status(&self) -> AuriaResult<OnChainSettlementStatus> {
        let pending_count = self.pending_receipts.read().await.len() as u32;
        
        let total_settled = {
            let submissions = self.submissions.read().await;
            submissions.iter()
                .filter(|s| s.status == SettlementSubmissionStatus::Confirmed)
                .map(|s| s.receipt_count as u64)
                .sum()
        };

        let last_submission = self.submissions.read().await.last().cloned();
        
        let block_number = self.eth_client.eth_block_number().await.ok();

        let pending_rewards = if self.is_connected().await {
            self.settlement_contract.get_reward(&self.wallet.address()).await
                .map(|r| u64::from_str_radix(r.trim_start_matches("0x"), 16).unwrap_or(0))
                .unwrap_or(0)
        } else {
            0
        };

        Ok(OnChainSettlementStatus {
            is_connected: *self.connected.read().await,
            chain_id: self.config.chain_id,
            wallet_address: self.wallet.address(),
            contract_address: self.config.settlement_contract_address.clone(),
            pending_receipts: pending_count,
            total_settled,
            last_settlement_block: last_submission.as_ref().and(Some(block_number)).flatten(),
            last_settlement_time: last_submission.map(|s| s.submitted_at),
            pending_rewards,
        })
    }

    pub async fn add_receipt(
        &self,
        request_id: RequestId,
        expert_ids: Vec<ExpertId>,
        usage: UsageStats,
    ) -> AuriaResult<String> {
        let receipt_id = self.generate_receipt_id(&request_id);
        
        let receipt = PendingReceipt {
            request_id,
            expert_ids,
            token_count: usage.tokens_generated,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };

        self.pending_receipts.write().await.push(receipt);

        tracing::debug!("Added receipt {} to pending settlement", receipt_id);

        if self.config.settle_on_threshold {
            let count = self.pending_receipts.read().await.len() as u32;
            if count >= self.config.threshold_receipts {
                tracing::info!("Threshold reached ({} receipts), triggering settlement", count);
                let _ = self.trigger_settlement().await;
            }
        }

        Ok(receipt_id)
    }

    fn generate_receipt_id(&self, request_id: &RequestId) -> String {
        let mut hasher = Keccak256::new();
        hasher.update(&request_id.0);
        hasher.update(SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_le_bytes());
        let hash = hasher.finalize();
        format!("0x{}", hex::encode(&hash[..16]))
    }

    pub async fn trigger_settlement(&self) -> AuriaResult<String> {
        if !self.is_connected().await {
            return Err(AuriaError::ExecutionError("Not connected to blockchain".to_string()));
        }

        let receipts = self.pending_receipts.read().await.clone();
        
        if receipts.len() < self.config.min_receipts_for_settlement as usize {
            return Err(AuriaError::ExecutionError(format!(
                "Not enough receipts for settlement (have {}, need {})",
                receipts.len(),
                self.config.min_receipts_for_settlement
            )));
        }

        let merkle_root = self.compute_merkle_root(&receipts)?;
        let receipt_ids: Vec<String> = receipts.iter()
            .map(|r| self.generate_receipt_id(&r.request_id))
            .collect();

        tracing::info!("Submitting settlement with {} receipts, root: {}", receipts.len(), merkle_root);

        let submission_id = format!("sub_{}", uuid::Uuid::new_v4());
        let submission = SettlementSubmission {
            submission_id: submission_id.clone(),
            tx_hash: String::new(),
            receipt_count: receipts.len() as u32,
            merkle_root: merkle_root.clone(),
            status: SettlementSubmissionStatus::Pending,
            submitted_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            confirmed_at: None,
            gas_used: None,
        };

        self.submissions.write().await.push(submission);

        let receipt = auria_blockchain::contracts::SettlementReceipt {
            receipt_id: submission_id.clone(),
            event_ids: receipt_ids,
            node_identity: self.wallet.address(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            signature: self.sign_receipt(&receipts)?,
        };

        match self.settlement_contract.submit_receipt(&receipt).await {
            Ok(tx_hash) => {
                let mut submissions = self.submissions.write().await;
                if let Some(sub) = submissions.last_mut() {
                    sub.tx_hash = tx_hash.clone();
                    sub.status = SettlementSubmissionStatus::Submitted;
                }

                let submission_idx = submissions.len() - 1;
                drop(submissions);

                self.confirm_submission(submission_idx).await?;

                self.pending_receipts.write().await.clear();

                tracing::info!("Settlement submitted successfully: {}", tx_hash);
                Ok(tx_hash)
            }
            Err(e) => {
                let mut submissions = self.submissions.write().await;
                if let Some(sub) = submissions.last_mut() {
                    sub.status = SettlementSubmissionStatus::Failed(e.to_string());
                }
                Err(e)
            }
        }
    }

    async fn confirm_submission(&self, submission_idx: usize) -> AuriaResult<()> {
        let tx_hash = {
            let submissions = self.submissions.read().await;
            submissions.get(submission_idx)
                .map(|s| s.tx_hash.clone())
                .unwrap_or_default()
        };

        if tx_hash.is_empty() {
            return Ok(());
        }

        let receipt = self.eth_client.wait_for_transaction(&tx_hash, 120).await?;

        let gas_used = u64::from_str_radix(
            receipt.gas_used.trim_start_matches("0x"), 
            16
        ).unwrap_or(0);

        let confirmed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut submissions = self.submissions.write().await;
        if let Some(sub) = submissions.get_mut(submission_idx) {
            sub.status = SettlementSubmissionStatus::Confirmed;
            sub.confirmed_at = Some(confirmed_at);
            sub.gas_used = Some(gas_used);
        }

        tracing::info!("Settlement confirmed on-chain, gas used: {}", gas_used);
        Ok(())
    }

    fn compute_merkle_root(&self, receipts: &[PendingReceipt]) -> AuriaResult<String> {
        if receipts.is_empty() {
            return Ok("0x".to_string());
        }

        let mut hashes: Vec<Vec<u8>> = receipts.iter()
            .map(|r| {
                let mut hasher = Keccak256::new();
                hasher.update(&r.request_id.0);
                hasher.update(r.token_count.to_le_bytes());
                hasher.update(r.timestamp.to_le_bytes());
                hasher.finalize().to_vec()
            })
            .collect();

        while hashes.len() > 1 {
            if hashes.len() % 2 != 0 {
                hashes.push(hashes.last().unwrap().clone());
            }
            hashes = hashes
                .chunks(2)
                .map(|chunk| {
                    let mut hasher = Keccak256::new();
                    hasher.update(&chunk[0]);
                    hasher.update(&chunk[1]);
                    hasher.finalize().to_vec()
                })
                .collect();
        }

        Ok(format!("0x{}", hex::encode(&hashes[0])))
    }

    fn sign_receipt(&self, receipts: &[PendingReceipt]) -> AuriaResult<String> {
        let mut data = Vec::new();
        for receipt in receipts {
            data.extend_from_slice(&receipt.request_id.0);
            data.extend_from_slice(&receipt.token_count.to_le_bytes());
        }

        let sig = self.wallet.sign_message(&data);
        Ok(format!("0x{}", sig.to_hex()))
    }

    pub async fn withdraw_rewards(&self) -> AuriaResult<String> {
        if !self.is_connected().await {
            return Err(AuriaError::ExecutionError("Not connected to blockchain".to_string()));
        }

        tracing::info!("Withdrawing rewards to {}", self.wallet.address());
        self.settlement_contract.withdraw().await
    }

    pub async fn record_usage(&self, amount: u64) -> AuriaResult<String> {
        if !self.is_connected().await {
            return Err(AuriaError::ExecutionError("Not connected to blockchain".to_string()));
        }

        self.settlement_contract.record_usage(&self.wallet.address(), amount).await
    }

    pub async fn get_pending_receipt_count(&self) -> usize {
        self.pending_receipts.read().await.len()
    }

    pub async fn get_submission_history(&self) -> Vec<SettlementSubmission> {
        self.submissions.read().await.clone()
    }

    pub async fn get_balance(&self) -> AuriaResult<u64> {
        if !self.is_connected().await {
            return Err(AuriaError::ExecutionError("Not connected to blockchain".to_string()));
        }

        self.eth_client.eth_get_balance(&self.wallet.address()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_on_chain_settlement_creation() {
        let config = OnChainSettlementConfig::default();
        let settlement = OnChainSettlement::new(config).await;
        assert!(settlement.is_ok());
    }

    #[test]
    fn test_receipt_id_generation() {
        let request_id = RequestId([1u8; 16]);
        let mut hasher = Keccak256::new();
        hasher.update(&request_id.0);
        let hash = hasher.finalize();
        assert_eq!(hash.len(), 32);
    }
}
