//! Integration tests for SigmaOS-omarchy-system-monitor.

use sigmaos_omarchy_system_monitor_lib::Component;

#[test]
fn component_initialises_and_runs() {
    let mut c = Component::new();
    c.init().expect("init should succeed");
    c.run().expect("run should succeed after init");
}
