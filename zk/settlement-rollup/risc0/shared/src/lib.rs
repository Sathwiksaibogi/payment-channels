use borsh::{BorshDeserialize, BorshSerialize};
use sha2::{Digest, Sha256};

pub const V1_PAYLOAD_LEN: usize = 50;
pub const V1_MAGIC: [u8; 2] = [0x56, 0x01];

/// PR #1 input: one existing V1 voucher authorization witness.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct V1VoucherInput {
    pub payload: Vec<u8>,
    pub authorized_signer: [u8; 32],
    pub signature: [u8; 64],
}

/// PR #1 public result after successful V1 authorization.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct V1VoucherJournal {
    pub channel_id: [u8; 32],
    pub cumulative_amount: u64,
    pub expires_at: i64,
}

/// Existing payment-channel `ChannelStatus::Open`.
pub const CHANNEL_STATUS_OPEN: u8 = 0;

/// Minimal channel state required to reproduce ordinary `settle` semantics.
///
/// This is intentionally smaller than the full 256-byte Channel account.
/// PR #3 will authenticate this logical state against the settlement
/// accumulator instead of trusting it as a standalone witness.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct ChannelStateInput {
    pub channel_address: [u8; 32],
    pub status: u8,
    pub deposit: u64,
    pub settled: u64,
    pub authorized_signer: [u8; 32],
}

/// One independently authorized settlement candidate.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SettlementInput {
    pub channel: ChannelStateInput,
    pub payload: Vec<u8>,
    pub signature: [u8; 64],
}

/// Input for validating multiple independent channel settlements.
///
/// `now_unix` is explicit for this prototype. It is not yet cryptographically
/// bound to Solana's Clock; that remains a later integration requirement.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SettlementBatchInput {
    pub now_unix: i64,
    pub settlements: Vec<SettlementInput>,
}

/// One valid logical advancement of a channel's settled watermark.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SettlementTransition {
    pub channel_address: [u8; 32],
    pub previous_settled: u64,
    pub new_settled: u64,
}

/// Domain separator for the canonical settlement-transition commitment.
pub const SETTLEMENT_BATCH_COMMITMENT_DOMAIN: &[u8] = b"zkchannel:settlement-batch:v1";

/// Compute the canonical, order-sensitive commitment to a validated
/// settlement batch.
///
/// Encoding:
/// domain
/// || settlement_count:u32 LE
/// || now_unix:i64 LE
/// || each(
///      channel_address:[u8;32]
///      || previous_settled:u64 LE
///      || new_settled:u64 LE
///    )
pub fn settlement_batch_commitment(
    now_unix: i64,
    transitions: &[SettlementTransition],
) -> [u8; 32] {
    let settlement_count = u32::try_from(transitions.len()).expect("settlement count exceeds u32");

    let mut hasher = Sha256::new();

    hasher.update(SETTLEMENT_BATCH_COMMITMENT_DOMAIN);
    hasher.update(settlement_count.to_le_bytes());
    hasher.update(now_unix.to_le_bytes());

    for transition in transitions {
        hasher.update(transition.channel_address);
        hasher.update(transition.previous_settled.to_le_bytes());
        hasher.update(transition.new_settled.to_le_bytes());
    }

    let digest = hasher.finalize();

    let mut commitment = [0u8; 32];
    commitment.copy_from_slice(&digest);
    commitment
}

/// Fixed-size public output of a successfully validated settlement batch.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SettlementBatchJournal {
    pub now_unix: i64,
    pub settlement_count: u32,
    pub transitions_commitment: [u8; 32],
}
