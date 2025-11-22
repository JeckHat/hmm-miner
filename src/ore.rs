use ore_api::{prelude::{Checkpoint, ClaimSOL, Deploy}, state::{automation_pda, board_pda, miner_pda, round_pda, treasury_pda}};
use steel::{AccountMeta, Instruction, Pubkey};

pub fn round_pda_ore(id: u64) -> (Pubkey, u8) {
    round_pda(id)
}

pub fn miner_pda_ore(authority: Pubkey) -> (Pubkey, u8) {
    miner_pda(authority)
}

pub fn board_pda_ore() -> (Pubkey, u8) {
    board_pda()
}

pub fn automation_pda_ore(authority: Pubkey) -> (Pubkey, u8) {
    automation_pda(authority)
}

pub fn deploy_ore(
    signer: Pubkey,
    authority: Pubkey,
    amount: u64,
    round_id: u64,
    squares: [bool; 25],
) -> Instruction {
    let automation_address = automation_pda_ore(authority).0;
    let board_address = board_pda_ore().0;
    let miner_address = miner_pda_ore(authority).0;
    let round_address = round_pda_ore(round_id).0;

    // Convert array of 25 booleans into a 32-bit mask where each bit represents whether
    // that square index is selected (1) or not (0)
    let mut mask: u32 = 0;
    for (i, &square) in squares.iter().enumerate() {
        if square {
            mask |= 1 << i;
        }
    }

    Instruction {
        program_id: ore_api::id(),
        accounts: vec![
            AccountMeta::new(signer, true),
            AccountMeta::new(authority, false),
            AccountMeta::new(automation_address, false),
            AccountMeta::new(board_address, false),
            AccountMeta::new(miner_address, false),
            AccountMeta::new(round_address, false),
            AccountMeta::new_readonly(Pubkey::from_str_const("11111111111111111111111111111111"), false),
        ],
        data: Deploy {
            amount: amount.to_le_bytes(),
            squares: mask.to_le_bytes(),
        }
        .to_bytes(),
    }
}

pub fn checkpoint_ore(signer: Pubkey, authority: Pubkey, round_id: u64) -> Instruction {
    let miner_address = miner_pda_ore(authority).0;
    let board_address = board_pda_ore().0;
    let round_address = round_pda_ore(round_id).0;
    let treasury_address = treasury_pda().0;
    Instruction {
        program_id: ore_api::id(),
        accounts: vec![
            AccountMeta::new(signer, true),
            AccountMeta::new(board_address, false),
            AccountMeta::new(miner_address, false),
            AccountMeta::new(round_address, false),
            AccountMeta::new(treasury_address, false),
            AccountMeta::new_readonly(Pubkey::from_str_const("11111111111111111111111111111111"), false),
        ],
        data: Checkpoint {}.to_bytes(),
    }
}

pub fn claim_sol_ore(signer: Pubkey) -> Instruction {
    let miner_address = miner_pda_ore(signer).0;
    Instruction {
        program_id: ore_api::id(),
        accounts: vec![
            AccountMeta::new(signer, true),
            AccountMeta::new(miner_address, false),
            AccountMeta::new_readonly(Pubkey::from_str_const("11111111111111111111111111111111"), false),
        ],
        data: ClaimSOL {}.to_bytes(),
    }
}
