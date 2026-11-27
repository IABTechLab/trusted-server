//! Portable record placement is declared independently of runtime switches.

use edgezero_core::app::{App, Hooks as _};
use edgezero_core::router::RouterService;
use trusted_server_adapter_axum::app::TrustedServerApp;

#[test]
fn ec_record_store_is_declared_without_environment_selection() {
    let mut app = App::new(RouterService::builder().build());
    TrustedServerApp::configure(&mut app).expect("should configure portable record placement");
    let layout = app
        .record_store_requirements()
        .get("trusted_server_kv")
        .expect("should declare the EC record layout without an enablement switch");
    assert_eq!(
        layout.id(),
        "trusted-server-ec",
        "should use the shared EC layout"
    );
    let stores = TrustedServerApp::stores()
        .kv
        .expect("should declare the logical EC record store");
    assert_eq!(
        stores.default, "trusted_server_kv",
        "should default to the EC store"
    );
    assert!(
        stores.ids.contains(&"trusted_server_kv"),
        "should publish the portable store ID"
    );
}
