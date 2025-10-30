#[cfg(test)]
pub mod tests {
    use crate::settings::Settings;

    pub fn crate_test_settings_str() -> String {
        r#"
            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_backend = "publisher_origin"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [prebid]
            server_url = "https://test-prebid.com/openrtb2/auction"  

            [synthetic] 
            counter_store = "test-counter-store"
            opid_store = "test-opid-store"
            secret_key = "test-secret-key"
            template = "{{client_ip}}:{{user_agent}}:{{first_party_id}}:{{auth_user_id}}:{{publisher_domain}}:{{accept_language}}"
            "#.to_string()
    }

    pub fn create_test_settings() -> Settings {
        let toml_str = crate_test_settings_str();
        Settings::from_toml(&toml_str).expect("Invalid config")
    }
}
