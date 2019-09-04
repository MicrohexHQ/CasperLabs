use std::collections::HashMap;

use crate::support::test_support::{PaymentCode, WasmTestBuilder, DEFAULT_BLOCK_TIME};
use contract_ffi::key::Key;
use contract_ffi::value::Account;
use contract_ffi::value::U512;
use engine_core::engine_state::MAX_PAYMENT;

const GENESIS_ADDR: [u8; 32] = [6u8; 32];
const ACCOUNT_1_ADDR: [u8; 32] = [1u8; 32];
const ACCOUNT_1_INITIAL_BALANCE: u64 = MAX_PAYMENT;

#[ignore]
#[test]
fn should_run_main_purse_contract_genesis_account() {
    let mut builder = WasmTestBuilder::default();

    let builder = builder.run_genesis(GENESIS_ADDR, HashMap::new());

    let genesis_account: Account = {
        let tmp = builder.clone();
        tmp.get_genesis_account().to_owned()
    };

    builder
        .exec_with_args(
            GENESIS_ADDR,
            "main_purse.wasm",
            DEFAULT_BLOCK_TIME,
            1,
            (
                genesis_account.purse_id(),
                U512::from(ACCOUNT_1_INITIAL_BALANCE),
            ),
        )
        .expect_success()
        .commit();
}

#[ignore]
#[test]
fn should_run_main_purse_contract_account_1() {
    let account_key = Key::Account(ACCOUNT_1_ADDR);

    let mut builder = WasmTestBuilder::default();

    let builder = builder
        .run_genesis(GENESIS_ADDR, HashMap::new())
        .exec_with_args(
            GENESIS_ADDR,
            "transfer_purse_to_account.wasm",
            DEFAULT_BLOCK_TIME,
            1,
            (ACCOUNT_1_ADDR, U512::from(ACCOUNT_1_INITIAL_BALANCE)),
        )
        .expect_success()
        .commit();

    let account_1: Account = {
        let tmp = builder.clone();
        let transforms = tmp.get_transforms();
        crate::support::test_support::get_account(&transforms[0], &account_key)
            .expect("should get account")
    };

    builder
        .exec_with_args(
            ACCOUNT_1_ADDR,
            "main_purse.wasm",
            DEFAULT_BLOCK_TIME,
            1,
            (account_1.purse_id(),),
        )
        .expect_success()
        .commit();
}
