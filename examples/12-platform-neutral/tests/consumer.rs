//! Confirms Core identity remains additive to normal application-owned state.

use rustclamp_example_platform_neutral::{TemperatureHistory, module_id};

#[test]
fn ordinary_rust_resource_survives_incremental_core_adoption() {
    let mut history = TemperatureHistory::default();
    history.record(20_500);
    history.record(20_750);
    assert_eq!(history.readings(), [20_500, 20_750]);
    assert_eq!(module_id().as_str(), "example.platform-neutral.thermometer");
}
