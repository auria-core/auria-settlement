// File: lib.rs - This file is part of AURIA
// Copyright (c) 2026 AURIA Developers and Contributors
// Description:
//     Usage accounting and settlement for AURIA Runtime Core.
//     Generates usage receipts and builds Merkle trees for settlement proof,
//     enabling economic attribution and royalty distribution.
//
use auria_core::{AuriaResult, Hash, UsageReceipt};
use sha3::{Digest, Keccak256};

pub struct SettlementClient {
    receipts: Vec<UsageReceipt>,
}

impl SettlementClient {
    pub fn new() -> Self {
        Self {
            receipts: Vec::new(),
        }
    }

    pub fn generate_receipt(&mut self, receipt: UsageReceipt) -> AuriaResult<Hash> {
        self.receipts.push(receipt.clone());
        let data = serde_json::to_vec(&receipt)
            .map_err(|e| auria_core::AuriaError::SerializationError(e.to_string()))?;
        let hash = Keccak256::digest(&data);
        let mut result = [0u8; 32];
        result.copy_from_slice(&hash);
        Ok(Hash(result))
    }

    pub fn submit_settlement(&self, _root_hash: Hash) -> AuriaResult<()> {
        Ok(())
    }

    pub fn build_merkle_tree(&self) -> AuriaResult<Hash> {
        if self.receipts.is_empty() {
            return Ok(Hash([0u8; 32]));
        }
        let mut hashes: Vec<Hash> = self
            .receipts
            .iter()
            .map(|r| {
                let data = serde_json::to_vec(r).unwrap_or_default();
                let hash = Keccak256::digest(&data);
                let mut result = [0u8; 32];
                result.copy_from_slice(&hash);
                Hash(result)
            })
            .collect();
        while hashes.len() > 1 {
            if hashes.len() % 2 != 0 {
                hashes.push(hashes.last().unwrap().clone());
            }
            hashes = hashes
                .chunks(2)
                .map(|chunk| {
                    let mut combined = Vec::new();
                    combined.extend_from_slice(&chunk[0].0);
                    combined.extend_from_slice(&chunk[1].0);
                    let hash = Keccak256::digest(&combined);
                    let mut result = [0u8; 32];
                    result.copy_from_slice(&hash);
                    Hash(result)
                })
                .collect();
        }
        Ok(hashes[0])
    }
}
