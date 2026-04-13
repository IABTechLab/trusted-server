// Stub implementation — full routing wired in Task 4.
use edgezero_core::app::Hooks;
use edgezero_core::router::RouterService;

pub struct TrustedServerApp;

impl Hooks for TrustedServerApp {
    fn routes() -> RouterService {
        RouterService::builder().build()
    }
}
