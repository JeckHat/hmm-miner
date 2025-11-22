use const_crypto::ed25519;
use ore_api::{consts::{AUTOMATION, BOARD, MINER, ROUND, TREASURY}, prelude::{Checkpoint, ClaimSOL, Deploy}};
use steel::{AccountMeta, Instruction, Pubkey};

pub const PROGRAM_ID: Pubkey = Pubkey::from_str_const("boreXQWsKpsJz5RR9BMtN8Vk4ndAk23sutj8spWYhwk");

const PROGRAM_ID_ORB: [u8; 32] = unsafe { *(&PROGRAM_ID as *const Pubkey as *const [u8; 32]) };

pub const BOARD_ADDRESS_ORB: Pubkey =
    Pubkey::new_from_array(ed25519::derive_program_address(&[BOARD], &PROGRAM_ID_ORB).0);

pub const TREASURY_ADDRESS_ORB: Pubkey =
    Pubkey::new_from_array(ed25519::derive_program_address(&[TREASURY], &PROGRAM_ID_ORB).0);

pub fn round_pda_orb(id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ROUND, &id.to_le_bytes()], &PROGRAM_ID)
}

pub fn miner_pda_orb(authority: Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[MINER, &authority.to_bytes()], &PROGRAM_ID)
}

pub fn board_pda_orb() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[BOARD], &PROGRAM_ID)
}

pub fn automation_pda_orb(authority: Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[AUTOMATION, &authority.to_bytes()], &PROGRAM_ID)
}

pub fn deploy_orb(
    signer: Pubkey,
    authority: Pubkey,
    amount: u64,
    round_id: u64,
    squares: [bool; 25],
) -> Instruction {
    let automation_address = automation_pda_orb(authority).0;
    let board_address = board_pda_orb().0;
    let miner_address = miner_pda_orb(authority).0;
    let round_address = round_pda_orb(round_id).0;

    // Convert array of 25 booleans into a 32-bit mask where each bit represents whether
    // that square index is selected (1) or not (0)
    let mut mask: u32 = 0;
    for (i, &square) in squares.iter().enumerate() {
        if square {
            mask |= 1 << i;
        }
    }

    Instruction {
        program_id: PROGRAM_ID,
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

pub fn checkpoint_orb(signer: Pubkey, authority: Pubkey, round_id: u64) -> Instruction {
    let miner_address = miner_pda_orb(authority).0;
    let board_address = board_pda_orb().0;
    let round_address = round_pda_orb(round_id).0;
    let treasury_address = TREASURY_ADDRESS_ORB;
    Instruction {
        program_id: PROGRAM_ID,
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

pub fn claim_sol_orb(signer: Pubkey) -> Instruction {
    let miner_address = miner_pda_orb(signer).0;
    Instruction {
        program_id: PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(signer, true),
            AccountMeta::new(miner_address, false),
            AccountMeta::new_readonly(Pubkey::from_str_const("11111111111111111111111111111111"), false),
        ],
        data: ClaimSOL {}.to_bytes(),
    }
}
