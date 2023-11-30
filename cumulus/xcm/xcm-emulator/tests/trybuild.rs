pub use rococo_system_emulated_network::RococoRelay;
use xcm_emulator;

type RuntimeEvent = RococoRelay::RuntimeEvent;
struct MockChain;
impl MockChain {}

#[test]
fn ui_tests() {
	let t = trybuild::TestCases::new();
	t.pass("tests/ui/expected_events.rs");
}
