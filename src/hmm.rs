use std::collections::VecDeque;

use rand::rngs::ThreadRng;
use rand::RngCore;

pub const M_OBS: usize = 25; // observations 0..24

#[derive(Clone)]
pub struct PoVAI {
    pub type_rng: usize,
    pub preds: Vec<usize>,
    pub total: usize,
    pub total_hit: usize,
    pub history: Vec<usize>,
    pub buffer: VecDeque<usize>,
    pub lose: u32,
    pub lose_in_row: u32,
    pub win: u32,
    pub win_in_row: u32,
    pub hmm_model: HMM,
    pub rounds_since_retrain: usize,
}

#[derive(Clone)]
pub struct HMM {
    pub n_states: usize,
    pub pi: Vec<f64>,           // size n
    pub a: Vec<Vec<f64>>,       // n x n
    pub b: Vec<Vec<f64>>,       // n x m
}

impl HMM {
    pub fn init() -> Self {
        Self {
            n_states: 0,
            pi: Vec::new(),
            a: Vec::new(),
            b: Vec::new(),
        }
    }
    /// Create random HMM. Uses rng.next_u64() internally.
    pub fn random(n_states: usize, m_obs: usize, rng: &mut ThreadRng) -> Self {
        let mut pi = vec![0.0; n_states];
        let mut a = vec![vec![0.0; n_states]; n_states];
        let mut b = vec![vec![0.0; m_obs]; n_states];

        // helper to produce f64 in (0,1]
        fn next_f64(rng: &mut ThreadRng) -> f64 {
            // map u64 to f64 in (0,1]
            let u = rng.next_u64();
            // convert to fraction in (0,1]
            let denom = std::u64::MAX as f64;
            // avoid zero by adding tiny eps
            (u as f64 / denom).max(1e-12)
        }

        // fill pi
        for i in 0..n_states {
            let x = next_f64(rng);
            pi[i] = x + 0.1; // small offset to avoid zeros
        }
        normalize_inplace(&mut pi);

        // fill a
        for i in 0..n_states {
            for j in 0..n_states {
                let x = next_f64(rng);
                a[i][j] = x + 0.1;
            }
            normalize_inplace(&mut a[i]);
        }

        // fill b
        for i in 0..n_states {
            for k in 0..m_obs {
                let x = next_f64(rng);
                b[i][k] = x + 0.1;
            }
            normalize_inplace(&mut b[i]);
        }

        HMM { n_states, pi, a, b }
    }
}

fn normalize_inplace(v: &mut [f64]) {
    let s: f64 = v.iter().sum();
    if s == 0.0 {
        let n = v.len();
        for x in v.iter_mut() { *x = 1.0 / n as f64; }
    } else {
        for x in v.iter_mut() { *x /= s; }
    }
}

/// Forward algorithm (scaling) -> returns alpha (T x N) and scales
fn forward_scaled(seq: &[usize], model: &HMM) -> (Vec<Vec<f64>>, Vec<f64>) {
    let n = model.n_states;
    let t_len = seq.len();
    let mut alpha = vec![vec![0.0; n]; t_len];
    let mut scales = vec![0.0; t_len];

    // t = 0
    let o0 = seq[0];
    for i in 0..n {
        alpha[0][i] = model.pi[i] * model.b[i][o0];
    }
    let s0: f64 = alpha[0].iter().sum();
    if s0 == 0.0 {
        for i in 0..n { alpha[0][i] = 1.0 / n as f64; }
        scales[0] = 1.0;
    } else {
        for i in 0..n { alpha[0][i] /= s0; }
        scales[0] = s0;
    }

    for t in 1..t_len {
        let ot = seq[t];
        for j in 0..n {
            let mut sum = 0.0;
            for i in 0..n {
                sum += alpha[t-1][i] * model.a[i][j];
            }
            alpha[t][j] = sum * model.b[j][ot];
        }
        let st: f64 = alpha[t].iter().sum();
        if st == 0.0 {
            for j in 0..n { alpha[t][j] = 1.0 / n as f64; }
            scales[t] = 1.0;
        } else {
            for j in 0..n { alpha[t][j] /= st; }
            scales[t] = st;
        }
    }

    (alpha, scales)
}

/// Backward algorithm scaled using scales from forward
fn backward_scaled(seq: &[usize], model: &HMM, scales: &[f64]) -> Vec<Vec<f64>> {
    let n = model.n_states;
    let t_len = seq.len();
    let mut beta = vec![vec![0.0; n]; t_len];

    for i in 0..n {
        beta[t_len-1][i] = 1.0;
    }
    for i in 0..n {
        beta[t_len-1][i] /= scales[t_len-1];
    }

    for t_rev in (0..t_len-1).rev() {
        let ot1 = seq[t_rev+1];
        for i in 0..n {
            let mut s = 0.0;
            for j in 0..n {
                s += model.a[i][j] * model.b[j][ot1] * beta[t_rev+1][j];
            }
            beta[t_rev][i] = s / scales[t_rev];
        }
    }
    beta
}

/// Baum-Welch training on one sequence
pub fn train_hmm(seq: &[usize], n_states: usize, n_iter: usize) -> HMM {
    let m_obs = M_OBS;
    let mut rng = rand::thread_rng();
    let mut model = HMM::random(n_states, m_obs, &mut rng);
    let t_len = seq.len();

    if t_len < 2 { return model; }

    for _iter in 0..n_iter {
        let mut pi_acc = vec![0.0; n_states];
        let mut a_acc = vec![vec![0.0; n_states]; n_states];
        let mut b_acc = vec![vec![0.0; m_obs]; n_states];

        let (alpha, scales) = forward_scaled(seq, &model);
        let beta = backward_scaled(seq, &model, &scales);

        let mut gamma = vec![vec![0.0; n_states]; t_len];
        for t in 0..t_len {
            let mut s = 0.0;
            for i in 0..n_states {
                gamma[t][i] = alpha[t][i] * beta[t][i];
                s += gamma[t][i];
            }
            if s == 0.0 {
                for i in 0..n_states { gamma[t][i] = 1.0 / n_states as f64; }
            } else {
                for i in 0..n_states { gamma[t][i] /= s; }
            }
        }

        for t in 0..(t_len-1) {
            let ot1 = seq[t+1];
            let mut denom = 0.0;
            for i in 0..n_states {
                for j in 0..n_states {
                    denom += alpha[t][i] * model.a[i][j] * model.b[j][ot1] * beta[t+1][j];
                }
            }
            if denom == 0.0 { denom = 1e-12; }
            for i in 0..n_states {
                for j in 0..n_states {
                    let numer = alpha[t][i] * model.a[i][j] * model.b[j][ot1] * beta[t+1][j];
                    let xi = numer / denom;
                    a_acc[i][j] += xi;
                }
            }
        }

        for i in 0..n_states {
            pi_acc[i] += gamma[0][i];
        }

        for t in 0..t_len {
            let ot = seq[t];
            for i in 0..n_states {
                b_acc[i][ot] += gamma[t][i];
            }
        }

        model.pi = pi_acc.clone();
        normalize_inplace(&mut model.pi);

        for i in 0..n_states {
            let row_sum: f64 = a_acc[i].iter().sum();
            if row_sum == 0.0 {
                for j in 0..n_states {
                    model.a[i][j] = 1.0 / n_states as f64;
                }
            } else {
                for j in 0..n_states {
                    model.a[i][j] = (a_acc[i][j] + 1e-6) / (row_sum + 1e-6 * n_states as f64);
                }
            }
        }

        for i in 0..n_states {
            let row_sum: f64 = b_acc[i].iter().sum();
            if row_sum == 0.0 {
                for k in 0..m_obs {
                    model.b[i][k] = 1.0 / m_obs as f64;
                }
            } else {
                for k in 0..m_obs {
                    model.b[i][k] = (b_acc[i][k] + 1e-6) / (row_sum + 1e-6 * m_obs as f64);
                }
            }
        }
    }

    model
}

/// Predict next observation distribution given last_window
pub fn predict_next_from_hmm(model: &HMM, last_window: &[usize]) -> Vec<f64> {
    if last_window.is_empty() {
        return vec![1.0 / M_OBS as f64; M_OBS];
    }
    let (alpha, _scales) = forward_scaled(last_window, model);
    let alpha_last = &alpha[alpha.len()-1];
    let n = model.n_states;
    let mut next_state = vec![0.0; n];
    for j in 0..n {
        let mut sum = 0.0;
        for i in 0..n {
            sum += alpha_last[i] * model.a[i][j];
        }
        next_state[j] = sum;
    }
    normalize_inplace(&mut next_state);

    let mut probs = vec![0.0; M_OBS];
    for o in 0..M_OBS {
        let mut s = 0.0;
        for j in 0..n {
            s += next_state[j] * model.b[j][o];
        }
        probs[o] = s;
    }
    normalize_inplace(&mut probs);
    probs
}

pub fn top_k_from_probs(probs: &[f64], k: usize) -> Vec<(usize, f64)> {
    let mut pairs: Vec<(usize, f64)> = probs.iter().cloned().enumerate().collect();
    pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    pairs.into_iter().take(k).collect()
}

pub fn online_update_emission(
    model: &mut HMM,
    last_window: &[usize],
    winning: usize,
    lr: f64,
) {
    let (alpha, _) = forward_scaled(last_window, model);
    let alpha_last = &alpha[alpha.len() - 1];

    for j in 0..model.n_states {
        model.b[j][winning] = model.b[j][winning] * (1.0 - lr) + alpha_last[j] * lr;
        if model.b[j][winning] < 1e-12 {
            model.b[j][winning] = 1e-12;
        }
    }

    for j in 0..model.n_states {
        normalize_inplace(&mut model.b[j]);
    }
}
