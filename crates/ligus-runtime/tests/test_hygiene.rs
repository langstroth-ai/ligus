//! Tests never reach the real user's sign-ins or keychain unless
//! `LIGUS_TEST_KEYCHAIN=1`. An integration test (and every other crate's
//! test) runs this crate with `cfg(test)` off, from a binary in cargo's
//! newer `build/<pkg>/<hash>/out/` layout: it must still count as a test.

use ligus_runtime::executor::cli_home;
use ligus_runtime::secret_store::{self, Backend};

#[test]
fn an_integration_test_binary_counts_as_a_test() {
    assert!(cli_home::in_test_binary(), "{:?}", std::env::current_exe());
}

#[test]
fn without_the_opt_in_no_real_sign_ins_and_no_os_secret_store() {
    if cli_home::test_keychain_opt_in() {
        eprintln!("skipped: LIGUS_TEST_KEYCHAIN=1");
        return;
    }
    assert!(
        !cli_home::real_sign_ins_allowed(),
        "a test would copy the user's CLI sign-ins"
    );
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        secret_store::open(dir.path(), None).backend(),
        Backend::File0600
    );
    assert_eq!(
        secret_store::init_for_data_dir(dir.path(), None).backend(),
        Backend::File0600
    );
}
