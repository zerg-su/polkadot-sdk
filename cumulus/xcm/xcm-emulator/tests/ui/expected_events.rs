use crate::xcm_emulator::TestContext;
use emulated_integration_tests_common::assert_expected_events;
type RuntimeEvent = RococoRelay::RuntimeEvent;
struct MockChain;
impl MockChain {}

fn main() {
	// set up XCM test context
	let account_id = AccountId32::new([0u8; 32]);
	let expected_amount = Balance::from(1000);
	assert_expected_events!(
		RococoRelay
		vec![
			RuntimeEvent::Balances(pallet_balances::Event::Withdraw { who, amount }) => {
				who: *who == account_id,
				amount: *amount == expected_amount,
			},
		]
	)
}
