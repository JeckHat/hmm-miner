use std::{collections::VecDeque, env, net::SocketAddr, sync::Arc, time::{Duration, Instant}};
use ore_api::state::{Board, Round};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use steel::AccountDeserialize;
use tokio::sync::{RwLock, broadcast};
use tracing;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::{hmm::{HMM, PoVAI, online_update_emission, predict_next_from_hmm, top_k_from_probs, train_hmm}, orb::{board_pda_orb, round_pda_orb}, ore::{board_pda_ore, round_pda_ore}, ws_server::build_router};

pub mod orb;
pub mod ore;
pub mod hmm;
pub mod ws_server;

const WINDOW: usize = 50usize;
const N_STATES: usize = 8usize;
const N_ITER: usize = 50usize;
const RETRAIN_EVERY: usize = 3usize;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().expect("Failed to load env");

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(env_filter)
        .init();

    let (tx, _rx) = broadcast::channel::<String>(200);
    let snapshot: Arc<RwLock<Option<ws_server::ServerSnapshot>>> =
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
        tracing::error!("[ORE] Board account not found");
    }

    if let Some(data) = board_orb_opt {
        let board_orb = Board::try_from_bytes(data)?;
        latest_round_orb = board_orb.round_id;
    } else {
        tracing::error!("[ORB] Board account not found");
    }

    let mut ore_ai = PoVAI {
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

    let mut orb_ai = PoVAI {
        type_rng: 1,
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

    latest_round_orb = latest_round_orb.saturating_sub(50);

    tracing::info!("Initialization...");
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

                tracing::info!("[ORE] Round: {} \nTotal_deployed: {}\nTime: {}\nSquare: {}\n\n",
                    i, round_ore.total_deployed, round_ore.expires_at, winning_square
                );
                ore_ai.history.push(winning_square);
            }
        } else {
            tracing::error!("[ORE] Round account not found");
        }

        if let Some(data) = round_orb_opt {
            let round_orb = Round::try_from_bytes(data)?;
            if let Some(rng) = round_orb.rng() {
                // winning square (0..24)
                let winning_square = round_orb.winning_square(rng) as usize;

                // writeln!(file, "{},{}", timestamp_str, id)?;
                tracing::info!("[ORB] Round: {} \nTotal_deployed: {}\nTime: {}\nSquare: {}\n\n",
                    latest_round_orb, round_orb.total_deployed, round_orb.expires_at, winning_square
                );
                orb_ai.history.push(winning_square);
            }
        } else {
            println!("[ORB] Round account not found");
        }
        latest_round_orb = latest_round_orb.saturating_add(1);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let conn_clone = Arc::clone(&connection);

    tokio::spawn(async move {
        update_loop(conn_clone, ore_ai, orb_ai).await;
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    let listener = tokio::net::TcpListener::bind(addr)
        .await?;

    axum::serve(listener, make_svc)
        .await
        .map_err(|e| anyhow::anyhow!("server error: {:?}", e))?;
    
    Ok(())
}

async fn update_loop(connection: Arc<RpcClient>, mut ore_ai: PoVAI, mut orb_ai: PoVAI) {
// async fn update() {
    tracing::info!("Running...");

    let mut last_deployed_round_ore: Option<u64> = None;
    let mut last_deployed_round_orb: Option<u64> = None;

    ore_ai.buffer = ore_ai.history.iter().cloned().collect();
    orb_ai.buffer = orb_ai.history.iter().cloned().collect();

    ore_ai.hmm_model = train_hmm(&ore_ai.history, N_STATES, N_ITER);
    orb_ai.hmm_model = train_hmm(&orb_ai.history, N_STATES, N_ITER);
    tracing::info!("[ORE] Initial HMM trained on {} observations.", &ore_ai.history.len());
    tracing::info!("[ORB] Initial HMM trained on {} observations.", &orb_ai.history.len());

    let mut ore_task_in_progress = false;
    let mut orb_task_in_progress = false;

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
                tracing::error!("Board ORE account not found");
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
                tracing::error!("Board ORB account not found");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        if last_deployed_round_ore != Some(board_ore.round_id) {
            last_deployed_round_ore = Some(board_ore.round_id);
            tracing::info!("[ORE] Round {}", board_ore.round_id);
            tracing::info!("[ORE] History {}: {:?}", board_ore.round_id, &ore_ai.history);

            let last_window: Vec<usize> = ore_ai.buffer.iter().cloned().collect();
            let probs = predict_next_from_hmm(&ore_ai.hmm_model, &last_window);
            let top = top_k_from_probs(&probs, 20usize);
            ore_ai.preds = top.iter().map(|(idx, _p)| *idx).collect();

        }

        if last_deployed_round_orb != Some(board_orb.round_id) {
            last_deployed_round_orb = Some(board_orb.round_id);
            tracing::info!("[ORB] round {}", board_orb.round_id);
            tracing::info!("[ORB] History {}: {:?}", board_orb.round_id, &orb_ai.history);

            let last_window: Vec<usize> = orb_ai.buffer.iter().cloned().collect();
            let probs = predict_next_from_hmm(&orb_ai.hmm_model, &last_window);
            let top = top_k_from_probs(&probs, 20usize);
            orb_ai.preds = top.iter().map(|(idx, _p)| *idx).collect();
        }

        let current_slot = if let Ok(s) = connection.get_slot().await { s } else {
            tracing::error!("Failed to get slot from rpc");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };


        tokio::time::sleep(Duration::from_secs(1)).await;

        let conn1 = Arc::clone(&connection);
        let conn2 = Arc::clone(&connection);
        let board_ore_cloned = board_ore.clone();
        let board_orb_cloned = board_orb.clone();
        let ore_ai_for_task = ore_ai.clone();
        let orb_ai_for_task = orb_ai.clone();

        tokio::spawn(async move {
            tokio::join!(
                update_rng(conn1, board_ore_cloned, ore_ai_for_task, current_slot),
                update_rng(conn2, board_orb_cloned, orb_ai_for_task, current_slot)
            );
        });

    }

}

async fn update_rng(connection: Arc<RpcClient>, board: Board, mut pov_ai: PoVAI) -> anyhow::Result<Round> {
    loop {

        let current_slot = match connection.get_slot().await {
            Ok(s) => s,
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        let last_deployable_slot = board.end_slot;
        let slots_left_in_round = last_deployable_slot as i64 - current_slot as i64;
        let round_key = if pov_ai.type_rng == 0 { &round_pda_ore(board.round_id).0 } else  { &round_pda_orb(board.round_id).0 }; 
        let start_wait = Instant::now();
        let tracing_type = if pov_ai.type_rng == 0 { "[ORE]".to_string() } else { "[ORB]".to_string() };

        if slots_left_in_round < 0 {
            let round: Round = loop {
                match connection.get_account_data(&round_key).await {
                    Ok(data) if !data.is_empty() => match Round::try_from_bytes(&data) {
                        Ok(r_ref) => {
                            let round_owned = r_ref.clone();
                            if let Some(rng) = round_owned.rng() {
                                tracing::info!("{} ✅ Round {} RNG available after {}s (rng={})", tracing_type, board.round_id, start_wait.elapsed().as_secs(), rng);
                                break round_owned;
                            } else {
                                tracing::info!("{} ⌛ Round {} still missing slot_hash... waiting 5s", tracing_type, board.round_id);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("{} ⚠️ Failed to parse Round {}: {:?}, retrying in 5s...", tracing_type, board.round_id, e);
                        }
                    },
                    Ok(_) => {
                        tracing::info!("{}ℹ️ Round account {} empty, waiting 5s...", tracing_type, board.round_id);
                    }
                    Err(e) => {
                        tracing::warn!("{} ⚠️ RPC error fetching round {}: {:?}, retrying in 5s...", tracing_type, board.round_id, e);
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            };

            if let Some(rng) = round.rng() {
                let winning_square = round.winning_square(rng) as usize;
                pov_ai.total += 1;
                tracing::info!("{} Round {} RNG present. rng={} \n Winning Square={}", tracing_type, round.id, rng, winning_square);
                let hit_pred = pov_ai.preds.contains(&winning_square);
                if hit_pred { pov_ai.total_hit += 1; };
                let wr = (pov_ai.total_hit as f64 / pov_ai.total as f64) * 100.0;
                tracing::info!("{} Result: {} | WR: {:.2}% ({}/{})\n", tracing_type, if hit_pred { "✅" } else { "❌" }, wr, pov_ai.total_hit, pov_ai.total);

                if pov_ai.buffer.len() >= WINDOW { pov_ai.buffer.pop_front(); }
                pov_ai.buffer.push_back(winning_square);
                pov_ai.history.push(winning_square);

                let last_window: Vec<usize> = pov_ai.buffer.iter().cloned().collect();

                if hit_pred {
                    if pov_ai.lose >= pov_ai.lose_in_row {
                        pov_ai.lose_in_row = pov_ai.lose;
                    }
                    pov_ai.lose = 0;
                    pov_ai.win += 1;
                    if pov_ai.win >= pov_ai.win_in_row {
                        pov_ai.win_in_row = pov_ai.win;
                    }
                    online_update_emission(&mut pov_ai.hmm_model, &last_window, winning_square, 0.01);
                } else {
                    pov_ai.lose += 1;
                    if pov_ai.lose >= pov_ai.lose_in_row {
                        pov_ai.lose_in_row = pov_ai.lose;
                    }
                    if pov_ai.win >= pov_ai.win_in_row {
                        pov_ai.win_in_row = pov_ai.win;
                    }
                    pov_ai.win = 0;
                    online_update_emission(&mut pov_ai.hmm_model, &last_window, winning_square, 0.06);
                }

                pov_ai.rounds_since_retrain += 1;
                if pov_ai.rounds_since_retrain >= RETRAIN_EVERY {
                    pov_ai.rounds_since_retrain = 0;
                    let train_seq = if pov_ai.history.len() > 50 { &pov_ai.history[pov_ai.history.len()-50..] } else { &pov_ai.history[..] };
                    tracing::info!("{} Retraining HMM ORE on {} obs...", tracing_type, train_seq.len());
                    pov_ai.hmm_model = train_hmm(train_seq, N_STATES, N_ITER);
                    tracing::info!("{}, Retrain done.", tracing_type);
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            } else {
                tracing::error!("{} Failed to get round rng for round {}", tracing_type, round.id);
            };
        } else {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
    }
}
