#[cfg(test)]
pub mod tests {
    use crate::settings::Settings;

    pub fn crate_test_settings_str() -> String {
        r#"
            [ad_server]
            ad_partner_backend = "https://test-adpartner.com"
            sync_url = "https://test-adpartner.com/synthetic_id={{synthetic_id}}"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_backend = "publisher_origin"
            origin_url= "https://origin.test-publisher.com"

            [prebid]
            server_url = "https://test-prebid.com/openrtb2/auction"

            [gam]
            publisher_id = "3790"
            server_url = "https://securepubads.g.doubleclick.net/gampad/ads"
            ad_units = [
                { name = "Flex8:1", size = "flexible" },
                { name = "Fixed728x90", size = "728x90" },
                { name = "Static8:1", size = "flexible" },
                { name = "Static728x90", size = "728x90" }
            ]

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
