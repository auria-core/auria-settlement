use auria_settlement::{
    SettlementClient, SettlementConfig, RoyaltyDistributor,
    PaymentStatus, EscrowAccount,
};
use auria_core::{RequestId, UsageStats, ExpertId, PublicKey};
use auria_settlement::Payment;
use uuid::Uuid;

#[tokio::test]
async fn test_settlement_client_creation() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let count = client.get_receipt_count().await;
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_generate_receipt() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let request_id = RequestId(Uuid::new_v4().into_bytes());
    let usage = UsageStats {
        tokens_generated: 100,
        tokens_processed: 50,
    };
    
    let result = client.generate_receipt(
        request_id,
        vec![],
        usage,
    ).await;
    
    assert!(result.is_ok());
    
    let count = client.get_receipt_count().await;
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_build_merkle_tree() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let root = client.build_merkle_tree().await;
    assert!(root.is_ok());
}

#[tokio::test]
async fn test_clear_receipts() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let request_id = RequestId(Uuid::new_v4().into_bytes());
    let usage = UsageStats {
        tokens_generated: 100,
        tokens_processed: 50,
    };
    
    client.generate_receipt(request_id, vec![], usage).await.unwrap();
    
    let count = client.get_receipt_count().await;
    assert_eq!(count, 1);
    
    client.clear_receipts().await;
    
    let count = client.get_receipt_count().await;
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_payment_creation() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let recipient = PublicKey([0u8; 32]);
    
    let payment = client.create_payment(recipient, 100).await;
    assert!(payment.is_ok());
    
    let payment = payment.unwrap();
    assert_eq!(payment.token_count, 100);
    assert_eq!(payment.status, PaymentStatus::Pending);
}

#[tokio::test]
async fn test_payment_processing() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let recipient = PublicKey([0u8; 32]);
    
    let payment = client.create_payment(recipient, 100).await.unwrap();
    
    let processed = client.process_payment(&payment.payment_id).await;
    assert!(processed.is_ok());
    
    let processed = processed.unwrap();
    assert_eq!(processed.status, PaymentStatus::Completed);
    assert!(processed.settled_at.is_some());
}

#[tokio::test]
async fn test_escrow_creation() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let owner = PublicKey([0u8; 32]);
    
    let account = client.create_escrow(owner, 10000).await;
    assert!(account.is_ok());
    
    let account = account.unwrap();
    assert_eq!(account.balance, 10000);
    assert_eq!(account.locked_amount, 0);
}

#[tokio::test]
async fn test_escrow_lock_funds() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let owner = PublicKey([0u8; 32]);
    
    let account = client.create_escrow(owner, 10000).await.unwrap();
    
    let result = client.lock_funds(&account.account_id, 5000).await;
    assert!(result.is_ok());
    
    let accounts = client.escrow_accounts.read().await;
    let updated = accounts.get(&account.account_id).unwrap();
    
    assert_eq!(updated.balance, 5000);
    assert_eq!(updated.locked_amount, 5000);
}

#[tokio::test]
async fn test_escrow_release_funds() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let owner = PublicKey([0u8; 32]);
    
    let account = client.create_escrow(owner, 10000).await.unwrap();
    
    client.lock_funds(&account.account_id, 5000).await.unwrap();
    client.release_funds(&account.account_id, 2000).await.unwrap();
    
    let accounts = client.escrow_accounts.read().await;
    let updated = accounts.get(&account.account_id).unwrap();
    
    assert_eq!(updated.balance, 7000);
    assert_eq!(updated.locked_amount, 3000);
}

#[tokio::test]
async fn test_insufficient_funds() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let owner = PublicKey([0u8; 32]);
    
    let account = client.create_escrow(owner, 1000).await.unwrap();
    
    let result = client.lock_funds(&account.account_id, 2000).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_royalty_calculation() {
    let distributor = RoyaltyDistributor::new(0.05);
    
    let (royalty, remaining) = distributor.calculate_royalties(1_000_000);
    
    assert_eq!(royalty, 50000);
    assert_eq!(remaining, 950000);
}

#[tokio::test]
async fn test_expert_distribution() {
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
async fn test_payment_amount_calculation() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let amount = client.calculate_payment_amount(100);
    
    let expected = (100.0 * config.token_price_usd * 1_000_000.0) as u64;
    assert_eq!(amount, expected);
}

#[tokio::test]
async fn test_royalty_split() {
    let config = SettlementConfig::default();
    let client = SettlementClient::new(config);
    
    let (royalty, remaining) = client.calculate_royalty_split(1_000_000);
    
    let expected_royalty = (1_000_000.0 * config.royalty_percentage) as u64;
    assert_eq!(royalty, expected_royalty);
    assert_eq!(remaining, 1_000_000 - expected_royalty);
}
