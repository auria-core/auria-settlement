# auria-settlement

Usage accounting and settlement for AURIA Runtime Core.

## Overview

Generates usage receipts and builds Merkle trees for settlement proof.

## Settlement Process

```
Receipts → Merkle Tree → Root submission → Claim distribution
```

## Usage

```rust
use auria_settlement::SettlementClient;

let mut client = SettlementClient::new();
let hash = client.generate_receipt(receipt)?;
let root = client.build_merkle_tree()?;
```
