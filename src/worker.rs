use std::{collections::VecDeque, env, net::SocketAddr, sync::Arc, time::Duration};
use ore_api::state::{Board, Round};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use steel::{AccountDeserialize, Pubkey};
use tokio::sync::{RwLock, broadcast};

use crate::{error_log, hmm::{HMM, PoVAI, online_update_emission, predict_next_from_hmm, top_k_from_probs, train_hmm}, info_log, orb::{self, board_pda_orb, round_pda_orb}, orb_log, ore::{board_pda_ore, round_pda_ore}, ore_log, rpc::{DeployOutcome, try_checkpoint_and_deploy_orb, try_checkpoint_and_deploy_ore, try_claim_sol_orb, try_claim_sol_ore}, warn_log, ws_server::{self, PoVAISnapShot, ServerSnapshot, WsServerHandle, build_router}};

const WINDOW: usize = 50usize;
const N_STATES: usize = 8usize;
const N_ITER: usize = 50usize;
const RETRAIN_EVERY: usize = 3usize;
const TOP_K: usize = 18usize;

pub const ORE_LOG: &str = "[ORE]";
pub const ORB_LOG: &str = "[ORB]";

pub async fn run_multiple(files: Vec<String>) -> anyhow::Result<()> {

    info_log!("Loaded {} miners", files.len());

    let (tx, _rx) = broadcast::channel::<String>(200);
    let snapshot: Arc<RwLock<Option<ServerSnapshot>>> =
        Arc::new(RwLock::new(None));

    let rpc_url = env::var("RPC_URL").expect("RPC_URL must be set");
    let prefix = "https://".to_string();
    let connection = Arc::new(RpcClient::new_with_commitment(
        prefix + &rpc_url,
        CommitmentConfig { commitment: CommitmentLevel::Confirmed },
    ));

    let ws_handle = ws_server::WsServerHandle {
        tx: tx.clone(),
        snapshot: snapshot.clone(),
    };

    let app = build_router(ws_handle.clone());
    let make_svc = app.into_make_service_with_connect_info::<SocketAddr>();
    // let connection = RpcClient::new_with_commitment(prefix + &rpc_url, CommitmentConfig { commitment: CommitmentLevel::Confirmed });

    let accounts = connection.get_multiple_accounts(&[board_pda_ore().0, board_pda_orb().0]).await?;
    let arr: [Option<_>; 2] = accounts
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected exactly 2 accounts from get_multiple_accounts"))?;

    let board_ore_opt = arr[0].as_ref().map(|acc| acc.data.as_slice());
    let board_orb_opt = arr[1].as_ref().map(|acc| acc.data.as_slice());

    let mut latest_round_ore = 0;
    let mut latest_round_orb = 0;

    if let Some(data) = board_ore_opt {
        let board_ore = Board::try_from_bytes(data)?;
        latest_round_ore = board_ore.round_id;
    } else {
        error_log!("{} Board account not found", ore_log());
    }

    if let Some(data) = board_orb_opt {
        let board_orb = Board::try_from_bytes(data)?;
        latest_round_orb = board_orb.round_id;
    } else {
        error_log!("{} Board account not found", orb_log());
    }

    let mut ore_ai = PoVAI::new();

    let mut orb_ai = PoVAI::new();

    latest_round_orb = latest_round_orb.saturating_sub(50);

    info_log!("Initialization...");
    let msg = Arc::new(RwLock::new(ServerSnapshot {
        r#type: "snapshot",
        rng_type: "multiple".to_string(),
        ore_snapshot: PoVAISnapShot {
            type_rng: 1,
            status: "init".to_string(),
            round: 0,
            preds: Vec::new(),
            total_round: 1,
            total_win: 0,
            win: 0,
            lose: 0,
            win_in_row: 0,
            lose_in_row: 0,
            winning_square: 0
        },
        orb_snapshot: PoVAISnapShot {
            type_rng: 1,
            status: "init".to_string(),
            round: 0,
            preds: Vec::new(),
            total_round: 1,
            total_win: 0,
            win: 0,
            lose: 0,
            win_in_row: 0,
            lose_in_row: 0,
            winning_square: 0
        }
    }));

    for i in latest_round_ore.saturating_sub(50)..=latest_round_ore + 2 {
        let round_key_ore = &round_pda_ore(i).0;
        let round_key_orb = &round_pda_orb(latest_round_orb).0;
        let accounts = connection.get_multiple_accounts(&[round_key_ore.clone(), round_key_orb.clone()]).await?;
        let arr: [Option<_>; 2] = accounts
            .try_into()
            .map_err(|_| anyhow::anyhow!("expected exactly 2 accounts from get_multiple_accounts"))?;

        let round_ore_opt = arr[0].as_ref().map(|acc| acc.data.as_slice());
        let round_orb_opt = arr[1].as_ref().map(|acc| acc.data.as_slice());

        if let Some(data) = round_ore_opt {
            let round_ore = Round::try_from_bytes(data)?;
            if let Some(rng) = round_ore.rng() {
                let winning_square = round_ore.winning_square(rng) as usize;
                info_log!("{} Round: {} | Total_deployed: {} | Time: {} | Square: {}\n",
                    ore_log(), i, round_ore.total_deployed, round_ore.expires_at, winning_square
                );
                ore_ai.history.push(winning_square);
            }
        } else {
            error_log!("{} Round account not found", ore_log());
        }

        if let Some(data) = round_orb_opt {
            let round_orb = Round::try_from_bytes(data)?;
            if let Some(rng) = round_orb.rng() {
                // winning square (0..24)
                let winning_square = round_orb.winning_square(rng) as usize;

                // writeln!(file, "{},{}", timestamp_str, id)?;
                info_log!("{} Round: {} | Total_deployed: {} | Time: {} | Square: {}\n",
                    orb_log(), latest_round_orb, round_orb.total_deployed, round_orb.expires_at, winning_square
                );
                orb_ai.history.push(winning_square);
            }
        } else {
            error_log!("{} Round account not found", orb_log());
        }
        latest_round_orb = latest_round_orb.saturating_add(1);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let conn_clone = Arc::clone(&connection);
    let msg_clone = msg.clone();
    let ws_clone = ws_handle.clone();

    tokio::spawn(async move {
        update_loop(conn_clone, ore_ai, orb_ai, msg_clone, ws_clone, files).await;
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    let listener = tokio::net::TcpListener::bind(addr)
        .await?;

    axum::serve(listener, make_svc)
        .await
        .map_err(|e| anyhow::anyhow!("server error: {:?}", e))?;
    
    Ok(())
}

async fn update_loop(connection: Arc<RpcClient>, mut ore_ai: PoVAI, mut orb_ai: PoVAI, msg: Arc<RwLock<ServerSnapshot>>, handle: WsServerHandle, files: Vec<String>) {
    info_log!("Running...");

    let mut last_round_ore: Option<u64> = None;
    let mut last_round_orb: Option<u64> = None;

    ore_ai.buffer = ore_ai.history.iter().cloned().collect();
    orb_ai.buffer = orb_ai.history.iter().cloned().collect();

    ore_ai.hmm_model = train_hmm(&ore_ai.history, N_STATES, N_ITER);
    orb_ai.hmm_model = train_hmm(&orb_ai.history, N_STATES, N_ITER);
    info_log!("{} Initial HMM trained on {} observations.", ore_log(), &ore_ai.history.len());
    info_log!("{} Initial HMM trained on {} observations.", orb_log(), &orb_ai.history.len());


    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        let accounts_res = connection
            .get_multiple_accounts(&[board_pda_ore().0, board_pda_orb().0])
            .await;

        // match result — **tidak** menaruh function call dalam pattern
        let accounts = if let Ok(a) = accounts_res {
            a
        } else {
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };

        let arr: [Option<_>; 2] = match accounts.try_into() {
            Ok(a) => a,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        let board_ore_opt = arr[0].as_ref();
        let board_orb_opt = arr[1].as_ref();

        let board_ore = match board_ore_opt {
            Some(acc) => match Board::try_from_bytes(&acc.data) {
                Ok(b) => b.clone(),
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            },
            None => {
                error_log!("Board ORE account not found");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        let board_orb = match board_orb_opt {
            Some(acc) => match Board::try_from_bytes(&acc.data) {
                Ok(b) => b.clone(),
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            },
            None => {
                error_log!("Board ORB account not found");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        let mut maybe_ore_handle: Option<tokio::task::JoinHandle<anyhow::Result<Round>>> = None;
        let mut maybe_orb_handle: Option<tokio::task::JoinHandle<anyhow::Result<Round>>> = None;

        if Some(board_ore.round_id) != last_round_ore {
            for file in &files {
                match try_claim_sol_ore(&connection, file).await {
                    Ok(DeployOutcome::Deployed(sig)) => {
                        info_log!("{} Claim submitted: {}", ore_log(), sig);
                    }
                    Ok(DeployOutcome::Skipped) => {
                        warn_log!("{} Skipped claim attempt for round {} - will retry next loop", ore_log(), board_ore.round_id);
                        continue;
                    }
                    Err(e) => {
                        error_log!("{} Unexpected error in checkpoint/deploy flow: {:?}", ore_log(), e);
                        continue;
                    }
                }
            }
            last_round_ore = Some(board_ore.round_id);
            info_log!("{} Round {}", ore_log(), board_ore.round_id);
            // prepare preds
            let last_window: Vec<usize> = ore_ai.buffer.iter().cloned().collect();
            let probs = predict_next_from_hmm(&ore_ai.hmm_model, &last_window);
            ore_ai.preds = top_k_from_probs(&probs, TOP_K).into_iter().map(|(i, _)| i).collect();

            let preds = ore_ai.preds.clone();

            {
                let mut snapshot = msg.write().await;
                snapshot.r#type = "snapshot";
                snapshot.ore_snapshot.status = "predicting".to_string();
                snapshot.ore_snapshot.round = board_ore.round_id as usize;
                snapshot.ore_snapshot.preds = preds;
                snapshot.ore_snapshot.total_round = if ore_ai.total == 0 { 1 } else { ore_ai.total };
                snapshot.ore_snapshot.total_win = ore_ai.total_hit;
                snapshot.ore_snapshot.win = ore_ai.win as usize;
                snapshot.ore_snapshot.lose = ore_ai.lose as usize;
                snapshot.ore_snapshot.win_in_row = ore_ai.win_in_row as usize;
                snapshot.ore_snapshot.lose_in_row = ore_ai.lose_in_row as usize;
            }
                
            {
                let snapshot = msg.read().await;
                if let Ok(json) = serde_json::to_string(&*snapshot) {
                    let _ = handle.tx.send(json);  // broadcast ke semua client
                    info_log!("Sent predictions via WS: {:?}", msg);
                }
            }

            let preds = ore_ai.preds.clone();

            let amount = if ore_ai.lose > 1 { 10_000 * 10u64.pow(ore_ai.lose) } else { 10_000 };
            for file in &files {
                match try_checkpoint_and_deploy_ore(&connection, board_ore.round_id, amount, &preds, file).await {
                    Ok(DeployOutcome::Deployed(sig)) => {
                        last_round_ore = Some(board_ore.round_id);
                        info_log!("{} Deployed for round {} sig {}", ore_log(), board_ore.round_id, sig);
                    }
                    Ok(DeployOutcome::Skipped) => {
                        error_log!("{} Skipped deploy attempt for round {} (path {}) - will retry next loop", ore_log(), board_ore.round_id, file);
                        continue;
                    }
                    Err(e) => {
                        error_log!("{} Unexpected error in checkpoint/deploy flow (path {}): {:?}", ore_log(), file, e);
                        continue;
                    }
                }
            }
        
            // spawn one-shot waiter for ORE round
            let conn = Arc::clone(&connection);
            let round_key = round_pda_ore(board_ore.round_id).0;
            maybe_ore_handle = Some(tokio::spawn(async move {
                wait_for_round_rng(conn, round_key, ore_log()).await
            }));
        }
        
        if Some(board_orb.round_id) != last_round_orb {
            for file in &files {
                match try_claim_sol_orb(&connection, file).await {
                    Ok(DeployOutcome::Deployed(sig)) => {
                        info_log!("{} Claim submitted: {}", orb_log(), sig);
                    }
                    Ok(DeployOutcome::Skipped) => {
                        warn_log!("{} Skipped claim attempt for round {} - will retry next loop", orb_log(), board_orb.round_id);
                        continue;
                    }
                    Err(e) => {
                        error_log!("{} Unexpected error in checkpoint/deploy flow: {:?}", orb_log(), e);
                        continue;
                    }
                }
            }
            last_round_orb = Some(board_orb.round_id);
            info_log!("{} Round {}", orb_log(), board_orb.round_id);
            let last_window: Vec<usize> = orb_ai.buffer.iter().cloned().collect();
            let probs = predict_next_from_hmm(&orb_ai.hmm_model, &last_window);
            orb_ai.preds = top_k_from_probs(&probs, TOP_K).into_iter().map(|(i, _)| i).collect();

            let preds = orb_ai.preds.clone();

            {
                let mut snapshot = msg.write().await;
                snapshot.r#type = "snapshot";
                snapshot.orb_snapshot.status = "predicting".to_string();
                snapshot.orb_snapshot.round = board_orb.round_id as usize;
                snapshot.orb_snapshot.preds = preds;
                snapshot.orb_snapshot.total_round = if orb_ai.total == 0 { 1 } else { orb_ai.total };
                snapshot.orb_snapshot.total_win = orb_ai.total_hit;
                snapshot.orb_snapshot.win = orb_ai.win as usize;
                snapshot.orb_snapshot.lose = orb_ai.lose as usize;
                snapshot.orb_snapshot.win_in_row = orb_ai.win_in_row as usize;
                snapshot.orb_snapshot.lose_in_row = orb_ai.lose_in_row as usize;
            }
                
            {
                let snapshot = msg.read().await;
                if let Ok(json) = serde_json::to_string(&*snapshot) {
                    let _ = handle.tx.send(json);  // broadcast ke semua client
                    info_log!("Sent predictions via WS: {:?}", msg);
                }
            }

            let preds = orb_ai.preds.clone();

            let amount = if orb_ai.lose > 1 { 10_000 * 10u64.pow(orb_ai.lose) } else { 10_000 };
            for file in &files {
                match try_checkpoint_and_deploy_orb(&connection, board_orb.round_id, amount, &preds, file).await {
                    Ok(DeployOutcome::Deployed(sig)) => {
                        last_round_orb = Some(board_orb.round_id);
                        info_log!("{} Deployed for round {} sig {}", orb_log(), board_orb.round_id, sig);
                    }
                    Ok(DeployOutcome::Skipped) => {
                        error_log!("{} Skipped deploy attempt for round {} (path {}) - will retry next loop", orb_log(), board_orb.round_id, file);
                        continue;
                    }
                    Err(e) => {
                        error_log!("{} Unexpected error in checkpoint/deploy flow (path {}): {:?}", orb_log(), file, e);
                        continue;
                    }
                }
            }
        
            // spawn one-shot waiter for ORB round
            let conn = Arc::clone(&connection);
            let round_key = round_pda_orb(board_orb.round_id).0;
            maybe_orb_handle = Some(tokio::spawn(async move {
                wait_for_round_rng(conn, round_key, orb_log()).await
            }));
        }
        
        // --- Await and process results (ORE) ---
        if let Some(h) = maybe_ore_handle {
            match h.await {
                Ok(Ok(round_ready)) => {
                    if let Some(rng) = round_ready.rng() {
                        let winning_square = round_ready.winning_square(rng) as usize;
                        ore_ai.total += 1;
                        let hit_pred = ore_ai.preds.contains(&winning_square);
                        if hit_pred { ore_ai.total_hit += 1; }
                        let wr = (ore_ai.total_hit as f64 / ore_ai.total as f64) * 100.0;
                        info_log!("{} Round {} RNG present (rng={}). WinningSquare={} WR={:.2}% ({}/{})",
                            ore_log(), round_ready.id, rng, winning_square, wr, ore_ai.total_hit, ore_ai.total
                        );
        
                        if ore_ai.buffer.len() >= WINDOW { ore_ai.buffer.pop_front(); }
                        ore_ai.buffer.push_back(winning_square);
                        ore_ai.history.push(winning_square);
        
                        let last_window: Vec<usize> = ore_ai.buffer.iter().cloned().collect();
                        if hit_pred {
                            ore_ai.win += 1;
                            if ore_ai.win >= ore_ai.win_in_row {
                                ore_ai.win_in_row = ore_ai.win;
                            }
                            if ore_ai.lose >= ore_ai.lose_in_row {
                                ore_ai.lose_in_row = ore_ai.lose;
                            }
                            ore_ai.lose = 0;
                            online_update_emission(&mut ore_ai.hmm_model, &last_window, winning_square, 0.01);
                        } else {
                            if ore_ai.win >= ore_ai.win_in_row {
                                ore_ai.win_in_row = ore_ai.win;
                            }
                            ore_ai.win = 0;
                            ore_ai.lose += 1;
                            if ore_ai.lose >= ore_ai.lose_in_row {
                                ore_ai.lose_in_row = ore_ai.lose;
                            }
                            online_update_emission(&mut ore_ai.hmm_model, &last_window, winning_square, 0.06);
                        }

                        info_log!("{} Round {} Winning Square: {}", ore_log(), round_ready.id, winning_square);
                        info_log!("{} Round {} Result: {} | WR:{:.2}% ({}/{})", ore_log(), round_ready.id, if hit_pred { "✅" } else { "❌" }, wr, ore_ai.total_hit, ore_ai.total);
                        info_log!("{} Win in a row: {}", ore_log(), ore_ai.win_in_row);
                        info_log!("{} Lose in a row: {}", ore_log(), ore_ai.lose_in_row);

                        {
                            let mut snapshot = msg.write().await;
                            snapshot.r#type = "snapshot";
                            snapshot.ore_snapshot.status = "result".to_string();
                            snapshot.ore_snapshot.round = board_ore.round_id as usize;
                            snapshot.ore_snapshot.total_round = if ore_ai.total == 0 { 1 } else { ore_ai.total };
                            snapshot.ore_snapshot.total_win = ore_ai.total_hit;
                            snapshot.ore_snapshot.win = ore_ai.win as usize;
                            snapshot.ore_snapshot.lose = ore_ai.lose as usize;
                            snapshot.ore_snapshot.win_in_row = ore_ai.win_in_row as usize;
                            snapshot.ore_snapshot.lose_in_row = ore_ai.lose_in_row as usize;
                            snapshot.ore_snapshot.winning_square = winning_square;
                        }
                            
                        {
                            let snapshot = msg.read().await;
                            if let Ok(json) = serde_json::to_string(&*snapshot) {
                                let _ = handle.tx.send(json);  // broadcast ke semua client
                                info_log!("Sent predictions via WS: {:?}", msg);
                            }
                        }
        
                        ore_ai.rounds_since_retrain += 1;
                        if ore_ai.rounds_since_retrain >= RETRAIN_EVERY {
                            ore_ai.rounds_since_retrain = 0;
                            let train_seq = if ore_ai.history.len() > WINDOW { &ore_ai.history[ore_ai.history.len()-WINDOW..] } else { &ore_ai.history[..] };
                            info_log!("{} Retraining HMM on {} obs...", ore_log(), train_seq.len());
                            ore_ai.hmm_model = train_hmm(train_seq, N_STATES, N_ITER);
                            info_log!("{} Retrain done.", ore_log());
                        }
                    }
                }
                Ok(Err(e)) => warn_log!("{} waiter failed: {:?}", ore_log(), e),
                Err(join_err) => warn_log!("{} waiter task panicked/cancelled: {:?}", ore_log(), join_err),
            }
        }
        
        // --- Await and process results (ORB) ---
        if let Some(h) = maybe_orb_handle {
            match h.await {
                Ok(Ok(round_ready)) => {
                    if let Some(rng) = round_ready.rng() {
                        let winning_square = round_ready.winning_square(rng) as usize;
                        orb_ai.total += 1;
                        let hit_pred = orb_ai.preds.contains(&winning_square);
                        if hit_pred { orb_ai.total_hit += 1; }
                        let wr = (orb_ai.total_hit as f64 / orb_ai.total as f64) * 100.0;
                        info_log!("{} Round {} RNG present (rng={}). WinningSquare={} WR={:.2}% ({}/{})",
                            orb_log(), round_ready.id, rng, winning_square, wr, orb_ai.total_hit, orb_ai.total
                        );
        
                        if orb_ai.buffer.len() >= WINDOW { orb_ai.buffer.pop_front(); }
                        orb_ai.buffer.push_back(winning_square);
                        orb_ai.history.push(winning_square);

                        let last_window: Vec<usize> = orb_ai.buffer.iter().cloned().collect();
                        if hit_pred {
                            orb_ai.win += 1;
                            if orb_ai.win >= orb_ai.win_in_row {
                                orb_ai.win_in_row = orb_ai.win;
                            }
                            if orb_ai.lose >= orb_ai.lose_in_row {
                                orb_ai.lose_in_row = orb_ai.lose;
                            }
                            orb_ai.lose = 0;
                            online_update_emission(&mut orb_ai.hmm_model, &last_window, winning_square, 0.01);
                        } else {
                            if orb_ai.win >= orb_ai.win_in_row {
                                orb_ai.win_in_row = orb_ai.win;
                            }
                            orb_ai.win = 0;
                            orb_ai.lose += 1;
                            if orb_ai.lose >= orb_ai.lose_in_row {
                                orb_ai.lose_in_row = orb_ai.lose;
                            }
                            online_update_emission(&mut orb_ai.hmm_model, &last_window, winning_square, 0.06);
                        }

                        info_log!("{} Round {} Winning Square: {}", orb_log(), round_ready.id, winning_square);
                        info_log!("{} Round {} Result: {} | WR:{:.2}% ({}/{})", orb_log(), round_ready.id, if hit_pred { "✅" } else { "❌" }, wr, orb_ai.total_hit, orb_ai.total);
                        info_log!("{} Win in a row: {}", orb_log(), orb_ai.win_in_row);
                        info_log!("{} Lose in a row: {}", orb_log(), orb_ai.lose_in_row);

                        {
                            let mut snapshot = msg.write().await;
                            snapshot.r#type = "snapshot";
                            snapshot.orb_snapshot.status = "result".to_string();
                            snapshot.orb_snapshot.round = board_orb.round_id as usize;
                            snapshot.orb_snapshot.total_round = if orb_ai.total == 0 { 1 } else { orb_ai.total };
                            snapshot.orb_snapshot.total_win = orb_ai.total_hit;
                            snapshot.orb_snapshot.win = orb_ai.win as usize;
                            snapshot.orb_snapshot.lose = orb_ai.lose as usize;
                            snapshot.orb_snapshot.win_in_row = orb_ai.win_in_row as usize;
                            snapshot.orb_snapshot.lose_in_row = orb_ai.lose_in_row as usize;
                            snapshot.orb_snapshot.winning_square = winning_square;
                        }
                            
                        {
                            let snapshot = msg.read().await;
                            if let Ok(json) = serde_json::to_string(&*snapshot) {
                                let _ = handle.tx.send(json);  // broadcast ke semua client
                                info_log!("Sent predictions via WS: {:?}", msg);
                            }
                        }
        
                        orb_ai.rounds_since_retrain += 1;
                        if orb_ai.rounds_since_retrain >= RETRAIN_EVERY {
                            orb_ai.rounds_since_retrain = 0;
                            let train_seq = if orb_ai.history.len() > WINDOW { &orb_ai.history[orb_ai.history.len()-WINDOW..] } else { &orb_ai.history[..] };
                            info_log!("{} Retraining HMM on {} obs...", orb_log(), train_seq.len());
                            orb_ai.hmm_model = train_hmm(train_seq, N_STATES, N_ITER);
                            info_log!("{} Retrain done.", orb_log());
                        }
                    }
                }
                Ok(Err(e)) => warn_log!("{} waiter failed: {:?}", orb_log(), e),
                Err(join_err) => warn_log!("{} waiter task panicked/cancelled: {:?}", orb_log(), join_err),
            }
        }
    }
}

async fn wait_for_round_rng(
    connection: Arc<RpcClient>,
    round_key: Pubkey,
    type_rng: colored::ColoredString
) -> anyhow::Result<Round> {
    loop {
        match connection.get_account_data(&round_key).await {
            Ok(data) if !data.is_empty() => {
                let round = Round::try_from_bytes(&data)?;
                if round.rng().is_some() {
                    return Ok(round.clone());
                } else {
                    info_log!("{} waiting 5s", type_rng)
                }
            }
            _ => { /* nothing */ }
        }

        tokio::time::sleep(Duration::from_secs(5 )).await;
    }
}

pub async fn run_single(files: Vec<String>, type_rng: usize) -> anyhow::Result<()> {

    info_log!("Loaded {} miners", files.len());

    let (tx, _rx) = broadcast::channel::<String>(200);
    let snapshot: Arc<RwLock<Option<ServerSnapshot>>> =
        Arc::new(RwLock::new(None));

    let rpc_url = env::var("RPC_URL").expect("RPC_URL must be set");
    let prefix = "https://".to_string();
    let connection = Arc::new(RpcClient::new_with_commitment(
        prefix + &rpc_url,
        CommitmentConfig { commitment: CommitmentLevel::Confirmed },
    ));

    let ws_handle = ws_server::WsServerHandle {
        tx: tx.clone(),
        snapshot: snapshot.clone(),
    };

    let app = build_router(ws_handle.clone());
    let make_svc = app.into_make_service_with_connect_info::<SocketAddr>();

    let board_adr = if type_rng == 0 { &board_pda_ore().0 } else { &board_pda_orb().0 };

    let board_data = connection.get_account_data(&board_adr).await?;
    let board = Board::try_from_bytes(&board_data)?;

    let rng_log = if type_rng == 0 { ore_log() } else { orb_log() };
    let mut latest_round = board.round_id;

    let mut pov_ai = PoVAI {
        type_rng: 0,
        preds: Vec::new(),
        total: 0,
        total_hit: 0,
        history: Vec::new(),
        buffer: VecDeque::<usize>::new(),
        lose: 0,
        lose_in_row: 0,
        win: 0,
        win_in_row: 0,
        hmm_model: HMM::init(),
        rounds_since_retrain: 0usize
    };

    info_log!("Initialization...");
    let rng_type_snapshot = if type_rng == 0 { "ore".to_string() } else { "orb".to_string() };
    let msg = Arc::new(RwLock::new(ServerSnapshot {
        r#type: "snapshot",
        rng_type: rng_type_snapshot,
        ore_snapshot: PoVAISnapShot::init(type_rng),
        orb_snapshot: PoVAISnapShot::init(type_rng + 1)
    }));

    for i in latest_round.saturating_sub(50)..=latest_round + 2 {
        let round_key = if type_rng == 0 { &round_pda_ore(i).0 } else { &round_pda_orb(i).0 };
        match connection.get_account_data(&round_key).await {
            Ok(data) => {
                match Round::try_from_bytes(&data) {
                    Ok(round) => {
                        if let Some(rng) = round.rng() {
                            let winning_square = round.winning_square(rng) as usize;
                            info_log!("{} Round: {} | Total_deployed: {} | Time: {} | Square: {}\n",
                                rng_log, i, round.total_deployed, round.expires_at, winning_square
                            );
                            pov_ai.history.push(winning_square);
                        }
                    },
                    Err(e) => {
                        error_log!("{} Round account not found", rng_log);
                    }
                }
            }
            Err(err) => {
                error_log!("{} Round account not found", rng_log);
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let conn_clone = Arc::clone(&connection);
    let msg_clone = msg.clone();
    let ws_clone = ws_handle.clone();

    tokio::spawn(async move {
        update_loop_single(conn_clone, type_rng, pov_ai, msg_clone, ws_clone, files).await;
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], if type_rng == 0 { 3000 } else { 3105 } ));

    let listener = tokio::net::TcpListener::bind(addr)
        .await?;

    axum::serve(listener, make_svc)
        .await
        .map_err(|e| anyhow::anyhow!("server error: {:?}", e))?;
    
    Ok(())
}

async fn update_loop_single(connection: Arc<RpcClient>, type_rng: usize, mut pov_ai: PoVAI, msg: Arc<RwLock<ServerSnapshot>>, handle: WsServerHandle, files: Vec<String>) {  
    let rng_log = if type_rng == 0 { ore_log() } else { orb_log() };
    info_log!("Running...");

    let mut last_round: Option<u64> = None;

    pov_ai.buffer = pov_ai.history.iter().cloned().collect();

    pov_ai.hmm_model = train_hmm(&pov_ai.history, N_STATES, N_ITER);
    info_log!("{} Initial HMM trained on {} observations.", rng_log, &pov_ai.history.len());

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;

        let board_adr = if type_rng == 0 { &board_pda_ore().0 } else { &board_pda_orb().0 };
        
        let board = if let Ok(data) = connection.get_account_data(&board_adr).await {
            if let Ok(b) = Board::try_from_bytes(&data) {
                b.clone()
            } else {
                error_log!("{} Failed to parse Board account", rng_log);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        } else {
            error_log!("{} Failed to load board account data", rng_log);
            tokio::time::sleep(Duration::from_secs(2)).await;
            continue;
        };

        let mut maybe_handle: Option<tokio::task::JoinHandle<anyhow::Result<Round>>> = None;
    
        if Some(board.round_id) != last_round {
            if type_rng == 0 {
                for file in &files {
                    match try_claim_sol_ore(&connection, file).await {
                        Ok(DeployOutcome::Deployed(sig)) => {
                            info_log!("{} Claim submitted: {}", rng_log, sig);
                        }
                        Ok(DeployOutcome::Skipped) => {
                            warn_log!("{} Skipped claim attempt for round {} - will retry next loop", rng_log, board.round_id);
                            continue;
                        }
                        Err(e) => {
                            error_log!("{} Unexpected error in checkpoint/deploy flow: {:?}", rng_log, e);
                            continue;
                        }
                    }
                }
            } else {
                for file in &files {
                    match try_claim_sol_orb(&connection, file).await {
                        Ok(DeployOutcome::Deployed(sig)) => {
                            info_log!("{} Claim submitted: {}", rng_log.clone(), sig);
                        }
                        Ok(DeployOutcome::Skipped) => {
                            warn_log!("{} Skipped claim attempt for round {} - will retry next loop", rng_log, board.round_id);
                            continue;
                        }
                        Err(e) => {
                            error_log!("{} Unexpected error in checkpoint/deploy flow: {:?}", rng_log, e);
                            continue;
                        }
                    }
                }
            }

            last_round = Some(board.round_id);
            info_log!("{} Round {}", rng_log, board.round_id);
            // prepare preds
            let last_window: Vec<usize> = pov_ai.buffer.iter().cloned().collect();
            let probs = predict_next_from_hmm(&pov_ai.hmm_model, &last_window);
            pov_ai.preds = top_k_from_probs(&probs, TOP_K).into_iter().map(|(i, _)| i).collect();
    
            let preds = pov_ai.preds.clone();

            if type_rng == 0 {
                {
                    let mut snapshot = msg.write().await;
                    snapshot.r#type = "snapshot";
                    snapshot.ore_snapshot.status = "predicting".to_string();
                    snapshot.ore_snapshot.round = board.round_id as usize;
                    snapshot.ore_snapshot.preds = preds;
                    snapshot.ore_snapshot.total_round = if pov_ai.total == 0 { 1 } else { pov_ai.total };
                    snapshot.ore_snapshot.total_win = pov_ai.total_hit;
                    snapshot.ore_snapshot.win = pov_ai.win as usize;
                    snapshot.ore_snapshot.lose = pov_ai.lose as usize;
                    snapshot.ore_snapshot.win_in_row = pov_ai.win_in_row as usize;
                    snapshot.ore_snapshot.lose_in_row = pov_ai.lose_in_row as usize;
                }
            } else {
                {
                    let mut snapshot = msg.write().await;
                    snapshot.r#type = "snapshot";
                    snapshot.orb_snapshot.status = "predicting".to_string();
                    snapshot.orb_snapshot.round = board.round_id as usize;
                    snapshot.orb_snapshot.preds = preds;
                    snapshot.orb_snapshot.total_round = if pov_ai.total == 0 { 1 } else { pov_ai.total };
                    snapshot.orb_snapshot.total_win = pov_ai.total_hit;
                    snapshot.orb_snapshot.win = pov_ai.win as usize;
                    snapshot.orb_snapshot.lose = pov_ai.lose as usize;
                    snapshot.orb_snapshot.win_in_row = pov_ai.win_in_row as usize;
                    snapshot.orb_snapshot.lose_in_row = pov_ai.lose_in_row as usize;
                }
            }
                    
            {
                let snapshot = msg.read().await;
                if let Ok(json) = serde_json::to_string(&*snapshot) {
                    let _ = handle.tx.send(json);  // broadcast ke semua client
                    info_log!("Sent predictions via WS: {:?}", msg);
                }
            }
    
            let preds = pov_ai.preds.clone();

            let amount = 10_000 * 10u64.pow(pov_ai.lose);
            if pov_ai.total < 50 || pov_ai.lose > 0 {
                if type_rng == 0 {
                    for file in &files {
                        match try_checkpoint_and_deploy_ore(&connection, board.round_id, amount, &preds, file).await {
                            Ok(DeployOutcome::Deployed(sig)) => {
                                last_round = Some(board.round_id);
                                info_log!("{} Deployed for round {} sig {}", rng_log, board.round_id, sig);
                            }
                            Ok(DeployOutcome::Skipped) => {
                                error_log!("{} Skipped deploy attempt for round {} (path {}) - will retry next loop", rng_log, board.round_id, file);
                                continue;
                            }
                            Err(e) => {
                                error_log!("{} Unexpected error in checkpoint/deploy flow (path {}): {:?}", rng_log, file, e);
                                continue;
                            }
                        }
                    }
                } else {
                    for file in &files {
                        match try_checkpoint_and_deploy_orb(&connection, board.round_id, amount, &preds, file).await {
                            Ok(DeployOutcome::Deployed(sig)) => {
                                last_round = Some(board.round_id);
                                info_log!("{} Deployed for round {} sig {}", rng_log, board.round_id, sig);
                            }
                            Ok(DeployOutcome::Skipped) => {
                                error_log!("{} Skipped deploy attempt for round {} (path {}) - will retry next loop", rng_log, board.round_id, file);
                                continue;
                            }
                            Err(e) => {
                                error_log!("{} Unexpected error in checkpoint/deploy flow (path {}): {:?}", rng_log, file, e);
                                continue;
                            }
                        }
                    }
                }
            }
            
            let conn = Arc::clone(&connection);
            
            let round_key = if type_rng == 0 { round_pda_ore(board.round_id).0 } else { round_pda_ore(board.round_id).0 };
            maybe_handle = Some(tokio::spawn(async move {
                wait_for_round_rng(conn, round_key, if type_rng == 0 { ore_log() } else { orb_log() }).await
            }));
        }
            
        if let Some(h) = maybe_handle {
            match h.await {
                Ok(Ok(round_ready)) => {
                    if let Some(rng) = round_ready.rng() {
                        let winning_square = round_ready.winning_square(rng) as usize;
                        pov_ai.total += 1;
                        let hit_pred = pov_ai.preds.contains(&winning_square);
                        if hit_pred { pov_ai.total_hit += 1; }
                        let wr = (pov_ai.total_hit as f64 / pov_ai.total as f64) * 100.0;
                        info_log!("{} Round {} RNG present (rng={}). WinningSquare={} WR={:.2}% ({}/{})",
                            rng_log, round_ready.id, rng, winning_square, wr, pov_ai.total_hit, pov_ai.total
                        );
        
                        if pov_ai.buffer.len() >= WINDOW { pov_ai.buffer.pop_front(); }
                        pov_ai.buffer.push_back(winning_square);
                        pov_ai.history.push(winning_square);
        
                        let last_window: Vec<usize> = pov_ai.buffer.iter().cloned().collect();
                        if hit_pred {
                            pov_ai.win += 1;
                            if pov_ai.win >= pov_ai.win_in_row {
                                pov_ai.win_in_row = pov_ai.win;
                            }
                            if pov_ai.lose >= pov_ai.lose_in_row {
                                pov_ai.lose_in_row = pov_ai.lose;
                            }
                            pov_ai.lose = 0;
                            online_update_emission(&mut pov_ai.hmm_model, &last_window, winning_square, 0.01);
                        } else {
                            if pov_ai.win >= pov_ai.win_in_row {
                                pov_ai.win_in_row = pov_ai.win;
                            }
                            pov_ai.win = 0;
                            pov_ai.lose += 1;
                            if pov_ai.lose >= pov_ai.lose_in_row {
                                pov_ai.lose_in_row = pov_ai.lose;
                            }
                            online_update_emission(&mut pov_ai.hmm_model, &last_window, winning_square, 0.06);
                        }

                        info_log!("{} Round {} Winning Square: {}", rng_log, round_ready.id, winning_square);
                        info_log!("{} Round {} Result: {} | WR:{:.2}% ({}/{})", rng_log, round_ready.id, if hit_pred { "✅" } else { "❌" }, wr, pov_ai.total_hit, pov_ai.total);
                        info_log!("{} Win in a row: {}", rng_log, pov_ai.win_in_row);
                        info_log!("{} Lose in a row: {}", rng_log, pov_ai.lose_in_row);

                        if type_rng == 0 {
                            {
                                let mut snapshot = msg.write().await;
                                snapshot.r#type = "snapshot";
                                snapshot.ore_snapshot.status = "result".to_string();
                                snapshot.ore_snapshot.round = board.round_id as usize;
                                snapshot.ore_snapshot.total_round = if pov_ai.total == 0 { 1 } else { pov_ai.total };
                                snapshot.ore_snapshot.total_win = pov_ai.total_hit;
                                snapshot.ore_snapshot.win = pov_ai.win as usize;
                                snapshot.ore_snapshot.lose = pov_ai.lose as usize;
                                snapshot.ore_snapshot.win_in_row = pov_ai.win_in_row as usize;
                                snapshot.ore_snapshot.lose_in_row = pov_ai.lose_in_row as usize;
                                snapshot.ore_snapshot.winning_square = winning_square;
                            }
                        } else {
                            {
                                let mut snapshot = msg.write().await;
                                snapshot.r#type = "snapshot";
                                snapshot.orb_snapshot.status = "result".to_string();
                                snapshot.orb_snapshot.round = board.round_id as usize;
                                snapshot.orb_snapshot.total_round = if pov_ai.total == 0 { 1 } else { pov_ai.total };
                                snapshot.orb_snapshot.total_win = pov_ai.total_hit;
                                snapshot.orb_snapshot.win = pov_ai.win as usize;
                                snapshot.orb_snapshot.lose = pov_ai.lose as usize;
                                snapshot.orb_snapshot.win_in_row = pov_ai.win_in_row as usize;
                                snapshot.orb_snapshot.lose_in_row = pov_ai.lose_in_row as usize;
                                snapshot.orb_snapshot.winning_square = winning_square;
                            }
                        }
                            
                        {
                            let snapshot = msg.read().await;
                            if let Ok(json) = serde_json::to_string(&*snapshot) {
                                let _ = handle.tx.send(json);  // broadcast ke semua client
                                info_log!("Sent predictions via WS: {:?}", msg);
                            }
                        }
        
                        pov_ai.rounds_since_retrain += 1;
                        if pov_ai.rounds_since_retrain >= RETRAIN_EVERY {
                            pov_ai.rounds_since_retrain = 0;
                            let train_seq = if pov_ai.history.len() > WINDOW { &pov_ai.history[pov_ai.history.len()-WINDOW..] } else { &pov_ai.history[..] };
                            info_log!("{} Retraining HMM on {} obs...", ore_log(), train_seq.len());
                            pov_ai.hmm_model = train_hmm(train_seq, N_STATES, N_ITER);
                            info_log!("{} Retrain done.", ore_log());
                        }
                    }
                }
                Ok(Err(e)) => warn_log!("{} waiter failed: {:?}", ore_log(), e),
                Err(join_err) => warn_log!("{} waiter task panicked/cancelled: {:?}", ore_log(), join_err),
            }
        }
    }
}