use std::time::Duration;

use anyhow::Result;
use ore_api::{sdk::claim_sol, state::Miner};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{commitment_config::CommitmentConfig, message::Message, signature::{Signature, read_keypair_file}, signer::Signer, transaction::Transaction};
use steel::{AccountDeserialize, Instruction};

use crate::{error_log, info_log, orb::{checkpoint_orb, claim_sol_orb, deploy_orb, miner_pda_orb}, orb_log, ore::{checkpoint_ore, claim_sol_ore, deploy_ore, miner_pda_ore}, ore_log, warn_log};

pub enum DeployOutcome {
    Deployed(Signature), // sukses, kembalikan signature
    Skipped,             // tidak jadi deploy -> lanjut loop utama (sama efek dengan `continue`)
}

pub async fn try_checkpoint_and_deploy_ore(
    connection: &RpcClient,
    board_round: u64,
    amount: u64,
    pred: &[usize],
    keyfile_path: &str,
) -> Result<DeployOutcome> {
    let signer_kp = read_keypair_file(keyfile_path.clone()).expect(format!("No keypair found at {}", keyfile_path).as_str());
    let signer_pubkey = signer_kp.pubkey();

    // 2) miner PDA & fetch miner on-chain (owned)
    let miner_adr = miner_pda_ore(signer_pubkey).0;
    let miner_data: Option<Miner> = match connection
        .get_account_with_commitment(&miner_adr, CommitmentConfig::confirmed())
        .await
    {
        Ok(resp) => {
            if let Some(acc) = resp.value {
                if acc.data.is_empty() {
                    error_log!("{} Miner account {} exists but empty.", ore_log(), signer_pubkey);
                    None
                } else {
                    match Miner::try_from_bytes(&acc.data) {
                        Ok(m_ref) => Some(m_ref.clone()),
                        Err(e) => {
                            error_log!("{} Failed parse miner {}: {:?}", ore_log(), signer_pubkey, e);
                            None
                        }
                    }
                }
            } else {
                info_log!("{} Miner account {} not found; will attempt checkpoint-create", ore_log(), signer_pubkey);
                None
            }
        }
        Err(e) => {
            error_log!("{} RPC error fetching miner {}: {:?}", ore_log(), signer_pubkey, e);
            return Ok(DeployOutcome::Skipped);
        }
    };

    let mut squares: [bool; 25] = [false; 25];
    for &idx in pred.iter() {
        if idx < squares.len() {
            squares[idx] = true;
        } else {
            error_log!("Pred out of range: {}", idx);
        }
    }

    // 4) decide checkpoint instruction(s)
    let mut ixs: Vec<Instruction> = Vec::new();
    if let Some(ref m) = miner_data {
        // peraturan di program: m.checkpoint_id == m.round_id diperlukan
        let miner_round_id = m.round_id;
        if m.checkpoint_id != miner_round_id {
            info_log!("{} Adding checkpoint(miner={} | round_id={})", ore_log(), signer_pubkey, miner_round_id);
            ixs.push(checkpoint_ore(signer_pubkey, signer_pubkey, miner_round_id));
        } else {
            info_log!("{} Miner {} already checkpointed for its round {}", ore_log(), signer_pubkey, miner_round_id);
            // tracing::info!("Miner already checkpointed for its round {}", miner_round_id);
        }
    } else {
        info_log!("{} Miner {} missing -> checkpoint(board_round={})", ore_log(), signer_pubkey, board_round);
        // tracing::info!("Miner missing -> checkpoint(board_round={})", board_round);
        ixs.push(checkpoint_ore(signer_pubkey, signer_pubkey, board_round));
    }

    // always add deploy
    ixs.push(deploy_ore(signer_pubkey, signer_pubkey, amount, board_round, squares));

    // 5) get blockhash (jika gagal -> skip iterasi)
    let recent_blockhash = match connection.get_latest_blockhash().await {
        Ok(bh) => bh,
        Err(e) => {
            error_log!("{} Failed to get recent blockhash: {:?}", ore_log(), e);
            return Ok(DeployOutcome::Skipped);
        }
    };

    let message = Message::new(&ixs, Some(&signer_pubkey));
    let mut tx = Transaction::new_unsigned(message);

    if let Err(e) = tx.try_sign(&[&signer_kp], recent_blockhash) {
        error_log!("{} Miner {} failed to sign tx: {:?}", ore_log(), signer_pubkey, e);
        return Ok(DeployOutcome::Skipped);
    }

    match connection.send_and_confirm_transaction(&tx).await {
        Ok(sig) => {
            info_log!("{} Miner {} Tx sent: {}", ore_log(), signer_pubkey, sig);
            // re-fetch miner finalized (cek checkpoint id) — jika mau
            // sleep sebentar agar validator index
            tokio::time::sleep(Duration::from_millis(1200)).await;
            match connection.get_account_with_commitment(&miner_adr, CommitmentConfig::finalized()).await {
                Ok(resp) => {
                    if let Some(acc) = resp.value {
                        if !acc.data.is_empty() {
                            if let Ok(miner_after) = Miner::try_from_bytes(&acc.data) {
                                println!("{} Miner {} after tx: checkpoint_id={}", ore_log(), signer_pubkey, miner_after.checkpoint_id);
                                // jika ingin sangat aman: hanya treat sebagai deployed jika checkpoint_id == board_round
                                // tapi kita tetap kembalikan signature karena tx sukses
                            }
                        }
                    }
                }
                Err(e) => error_log!("{} Failed to re-fetch miner {} after tx: {:?}", ore_log(), signer_pubkey, e),
            }

            return Ok(DeployOutcome::Deployed(sig));
        }
        Err(e) => {
            error_log!("{} Failed to send tx ({}): {:?}", ore_log(), signer_pubkey, e);
            return Ok(DeployOutcome::Skipped);
        }
    }
}

pub async fn try_checkpoint_and_deploy_orb(
    connection: &RpcClient,
    board_round: u64,
    amount: u64,
    pred: &[usize],
    keyfile_path: &str,
) -> Result<DeployOutcome> {
    let signer_kp = read_keypair_file(keyfile_path.clone()).expect(format!("No keypair found at {}", keyfile_path).as_str());
    let signer_pubkey = signer_kp.pubkey();

    // 2) miner PDA & fetch miner on-chain (owned)
    let miner_adr = miner_pda_orb(signer_pubkey).0;
    let miner_data: Option<Miner> = match connection
        .get_account_with_commitment(&miner_adr, CommitmentConfig::confirmed())
        .await
    {
        Ok(resp) => {
            if let Some(acc) = resp.value {
                if acc.data.is_empty() {
                    error_log!("{} Miner account {} exists but empty.", orb_log(), signer_pubkey);
                    None
                } else {
                    match Miner::try_from_bytes(&acc.data) {
                        Ok(m_ref) => Some(m_ref.clone()),
                        Err(e) => {
                            error_log!("{} Failed parse miner {}: {:?}", orb_log(), signer_pubkey, e);
                            None
                        }
                    }
                }
            } else {
                info_log!("{} Miner account {} not found; will attempt checkpoint-create", orb_log(), signer_pubkey);
                None
            }
        }
        Err(e) => {
            error_log!("{} RPC error fetching miner {}: {:?}", orb_log(), signer_pubkey, e);
            return Ok(DeployOutcome::Skipped);
        }
    };

    let mut squares: [bool; 25] = [false; 25];
    for &idx in pred.iter() {
        if idx < squares.len() {
            squares[idx] = true;
        } else {
            error_log!("Pred out of range: {}", idx);
        }
    }

    // 4) decide checkpoint instruction(s)
    let mut ixs: Vec<Instruction> = Vec::new();
    if let Some(ref m) = miner_data {
        // peraturan di program: m.checkpoint_id == m.round_id diperlukan
        let miner_round_id = m.round_id;
        if m.checkpoint_id != miner_round_id {
            info_log!("{} Adding checkpoint(miner={} | round_id={})", orb_log(), signer_pubkey, miner_round_id);
            ixs.push(checkpoint_orb(signer_pubkey, signer_pubkey, miner_round_id));
        } else {
            info_log!("{} Miner {} already checkpointed for its round {}", orb_log(), signer_pubkey, miner_round_id);
            // tracing::info!("Miner already checkpointed for its round {}", miner_round_id);
        }
    } else {
        info_log!("{} Miner {} missing -> checkpoint(board_round={})", orb_log(), signer_pubkey, board_round);
        // tracing::info!("Miner missing -> checkpoint(board_round={})", board_round);
        ixs.push(checkpoint_orb(signer_pubkey, signer_pubkey, board_round));
    }

    // always add deploy
    ixs.push(deploy_orb(signer_pubkey, signer_pubkey, amount, board_round, squares));

    // 5) get blockhash (jika gagal -> skip iterasi)
    let recent_blockhash = match connection.get_latest_blockhash().await {
        Ok(bh) => bh,
        Err(e) => {
            error_log!("{} Failed to get recent blockhash: {:?}", orb_log(), e);
            return Ok(DeployOutcome::Skipped);
        }
    };

    let message = Message::new(&ixs, Some(&signer_pubkey));
    let mut tx = Transaction::new_unsigned(message);

    if let Err(e) = tx.try_sign(&[&signer_kp], recent_blockhash) {
        error_log!("{} Miner {} failed to sign tx: {:?}", orb_log(), signer_pubkey, e);
        return Ok(DeployOutcome::Skipped);
    }

    match connection.send_and_confirm_transaction(&tx).await {
        Ok(sig) => {
            info_log!("{} Miner {} Tx sent: {}", orb_log(), signer_pubkey, sig);
            // re-fetch miner finalized (cek checkpoint id) — jika mau
            // sleep sebentar agar validator index
            tokio::time::sleep(Duration::from_millis(1200)).await;
            match connection.get_account_with_commitment(&miner_adr, CommitmentConfig::finalized()).await {
                Ok(resp) => {
                    if let Some(acc) = resp.value {
                        if !acc.data.is_empty() {
                            if let Ok(miner_after) = Miner::try_from_bytes(&acc.data) {
                                println!("{} Miner {} after tx: checkpoint_id={}", orb_log(), signer_pubkey, miner_after.checkpoint_id);
                                // jika ingin sangat aman: hanya treat sebagai deployed jika checkpoint_id == board_round
                                // tapi kita tetap kembalikan signature karena tx sukses
                            }
                        }
                    }
                }
                Err(e) => error_log!("{} Failed to re-fetch miner {} after tx: {:?}", orb_log(), signer_pubkey, e),
            }

            return Ok(DeployOutcome::Deployed(sig));
        }
        Err(e) => {
            error_log!("{} Failed to send tx ({}): {:?}", orb_log(), signer_pubkey, e);
            return Ok(DeployOutcome::Skipped);
        }
    }
}

pub async fn try_claim_sol_ore(
    connection: &RpcClient,
    keyfile_path: &str
) -> Result<DeployOutcome> {
    let signer_kp = read_keypair_file(keyfile_path.clone()).expect(format!("No keypair found at {}", keyfile_path).as_str());
    let signer_pubkey = signer_kp.pubkey();

    let miner_adr = miner_pda_ore(signer_pubkey).0;

    let miner_opt: Option<Miner> = match connection
        .get_account_with_commitment(&miner_adr, CommitmentConfig::confirmed())
        .await
    {
        Ok(resp) => {
            if let Some(acc) = resp.value {
                if acc.data.is_empty() {
                    info_log!("{} Miner account {} exists but data empty", ore_log(), signer_pubkey);
                    None
                } else {
                    match Miner::try_from_bytes(&acc.data) {
                        Ok(m_ref) => Some(m_ref.clone()),
                        Err(e) => {
                            error_log!("{} Failed to deserialize Miner {}: {:?}", ore_log(), signer_pubkey, e);
                            None
                        }
                    }
                }
            } else {
                error_log!("{} Miner account {} not found on-chain", ore_log(), signer_pubkey);
                None
            }
        }
        Err(e) => {
            error_log!("{} RPC error fetching miner {}: {:?}", ore_log(), signer_pubkey, e);
            return Err(anyhow::anyhow!("RPC error fetching miner: {:?}", e));
        }
    };

    let miner = match miner_opt {
        Some(m) => m,
        None => {
            error_log!("{} No miner account / no data => nothing to claim", ore_log());
            return Ok(DeployOutcome::Skipped);
        }
    };

    if miner.rewards_sol == 0 {
        warn_log!("{} Miner {} has 0 rewards_sol -> skipping claim", ore_log(), signer_pubkey);
        return Ok(DeployOutcome::Skipped);
    }

    let ix: Instruction = claim_sol_ore(signer_pubkey);

    let recent_blockhash = match connection.get_latest_blockhash().await {
        Ok(bh) => bh,
        Err(e) => {
            error_log!("{} Miner {} failed to get recent blockhash for claim: {:?}", ore_log(), signer_pubkey, e);
            // tracing::error!("Failed to get recent blockhash for claim: {:?}", e);
            return Err(anyhow::anyhow!("Failed to get recent blockhash: {:?}", e));
        }
    };

    // Create message & transaction
    let message = Message::new(&[ix], Some(&signer_pubkey));
    let mut tx = Transaction::new_unsigned(message);

    if let Err(e) = tx.try_sign(&[&signer_kp], recent_blockhash) {
        error_log!("{} Miner {} failed to sign claim transaction: {:?}", ore_log(), signer_pubkey, e);
        return Err(anyhow::anyhow!("Failed to sign claim tx: {:?}", e));
    }

    match connection.send_and_confirm_transaction(&tx).await {
        Ok(sig) => {
            info_log!("{} Claim SOL tx sent for {}: {}", ore_log(), signer_pubkey, sig);
            tokio::time::sleep(Duration::from_millis(1200)).await;
            match connection.get_account_with_commitment(&miner_adr, CommitmentConfig::finalized()).await {
                Ok(resp) => {
                    if let Some(acc) = resp.value {
                        if !acc.data.is_empty() {
                            if let Ok(miner_after) = Miner::try_from_bytes(&acc.data) {
                                info_log!("{} Miner {} after claim: rewards_sol = {}", ore_log(), signer_pubkey, miner_after.rewards_sol);
                            }
                        }
                    }
                }
                Err(e) => error_log!("{} Miner {} failed to re-fetch miner after claim: {:?}", ore_log(), signer_pubkey, e),
            }

            Ok(DeployOutcome::Deployed(sig))
        }
        Err(e) => {
            error_log!("{} Miner {} failed to send claim tx: {:?}", ore_log(), signer_pubkey, e);
            Err(anyhow::anyhow!("Failed to send claim tx: {:?}", e))
        }
    }
}

pub async fn try_claim_sol_orb(
    connection: &RpcClient,
    keyfile_path: &str
) -> Result<DeployOutcome> {
    let signer_kp = read_keypair_file(keyfile_path.clone()).expect(format!("No keypair found at {}", keyfile_path).as_str());
    let signer_pubkey = signer_kp.pubkey();

    let miner_adr = miner_pda_orb(signer_pubkey).0;

    let miner_opt: Option<Miner> = match connection
        .get_account_with_commitment(&miner_adr, CommitmentConfig::confirmed())
        .await
    {
        Ok(resp) => {
            if let Some(acc) = resp.value {
                if acc.data.is_empty() {
                    info_log!("{} Miner account {} exists but data empty", orb_log(), signer_pubkey);
                    None
                } else {
                    match Miner::try_from_bytes(&acc.data) {
                        Ok(m_ref) => Some(m_ref.clone()),
                        Err(e) => {
                            error_log!("{} Failed to deserialize Miner {}: {:?}", orb_log(), signer_pubkey, e);
                            None
                        }
                    }
                }
            } else {
                error_log!("{} Miner account {} not found on-chain", orb_log(), signer_pubkey);
                None
            }
        }
        Err(e) => {
            error_log!("{} RPC error fetching miner {}: {:?}", orb_log(), signer_pubkey, e);
            return Err(anyhow::anyhow!("RPC error fetching miner: {:?}", e));
        }
    };

    let miner = match miner_opt {
        Some(m) => m,
        None => {
            error_log!("{} No miner account / no data => nothing to claim", orb_log());
            return Ok(DeployOutcome::Skipped);
        }
    };

    if miner.rewards_sol == 0 {
        warn_log!("{} Miner {} has 0 rewards_sol -> skipping claim", orb_log(), signer_pubkey);
        return Ok(DeployOutcome::Skipped);
    }

    let ix: Instruction = claim_sol_orb(signer_pubkey);

    let recent_blockhash = match connection.get_latest_blockhash().await {
        Ok(bh) => bh,
        Err(e) => {
            error_log!("{} Miner {} failed to get recent blockhash for claim: {:?}", orb_log(), signer_pubkey, e);
            // tracing::error!("Failed to get recent blockhash for claim: {:?}", e);
            return Err(anyhow::anyhow!("Failed to get recent blockhash: {:?}", e));
        }
    };

    // Create message & transaction
    let message = Message::new(&[ix], Some(&signer_pubkey));
    let mut tx = Transaction::new_unsigned(message);

    if let Err(e) = tx.try_sign(&[&signer_kp], recent_blockhash) {
        error_log!("{} Miner {} failed to sign claim transaction: {:?}", orb_log(), signer_pubkey, e);
        return Err(anyhow::anyhow!("Failed to sign claim tx: {:?}", e));
    }

    match connection.send_and_confirm_transaction(&tx).await {
        Ok(sig) => {
            info_log!("{} Claim SOL tx sent for {}: {}", orb_log(), signer_pubkey, sig);
            tokio::time::sleep(Duration::from_millis(1200)).await;
            match connection.get_account_with_commitment(&miner_adr, CommitmentConfig::finalized()).await {
                Ok(resp) => {
                    if let Some(acc) = resp.value {
                        if !acc.data.is_empty() {
                            if let Ok(miner_after) = Miner::try_from_bytes(&acc.data) {
                                info_log!("{} Miner {} after claim: rewards_sol = {}", orb_log(), signer_pubkey, miner_after.rewards_sol);
                            }
                        }
                    }
                }
                Err(e) => error_log!("{} Miner {} failed to re-fetch miner after claim: {:?}", orb_log(), signer_pubkey, e),
            }

            Ok(DeployOutcome::Deployed(sig))
        }
        Err(e) => {
            error_log!("{} Miner {} failed to send claim tx: {:?}", orb_log(), signer_pubkey, e);
            Err(anyhow::anyhow!("Failed to send claim tx: {:?}", e))
        }
    }
}
