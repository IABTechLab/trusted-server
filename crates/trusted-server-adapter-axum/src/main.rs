use trusted_server_adapter_axum::app::TrustedServerApp;

fn main() {
    if let Err(err) =
        edgezero_adapter_axum::run_app::<TrustedServerApp>(include_str!("../axum.toml"))
    {
        eprintln!("trusted-server-adapter-axum failed: {err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compiles() {}
}
