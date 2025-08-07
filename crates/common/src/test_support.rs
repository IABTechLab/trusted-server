#[cfg(test)]
pub mod tests {
    use crate::settings::Settings;

    pub fn crate_test_settings_str() -> String {
        r#"
            [ad_server]
            ad_partner_url = "https://test-adpartner.com"
            sync_url = "https://test-adpartner.com/synthetic_id={{synthetic_id}}"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url= "https://origin.test-publisher.com"

            [prebid]
            server_url = "https://test-prebid.com/openrtb2/auction"

            [gam]
            publisher_id = "21796327522"
            server_url = "https://securepubads.g.doubleclick.net/gampad/ads"
            ad_units = [
                { name = "test_unit_1", size = "320x50" },
                { name = "test_unit_2", size = "728x90" },
            ]       

            [synthetic] 
            counter_store = "test-counter-store"
            opid_store = "test-opid-store"
            secret_key = "test-secret-key"
            template = "{{client_ip}}:{{user_agent}}:{{first_party_id}}:{{auth_user_id}}:{{publisher_domain}}:{{accept_language}}"

            [partners]
            [partners.gam]
            enabled = true
            name = "Google Ad Manager"
            domains_to_proxy = [
                "securepubads.g.doubleclick.net",
                "tpc.googlesyndication.com",
            ]
            proxy_domain = "creatives.auburndao.com"
            backend_name = "gam_proxy_backend"

            [partners.equativ]
            enabled = true
            name = "Equativ (Smart AdServer)"
            domains_to_proxy = [
                "creatives.sascdn.com"
            ]
            proxy_domain = "creatives.auburndao.com"
            backend_name = "equativ_proxy_backend"
            "#.to_string()
    }

    pub fn create_test_settings() -> Settings {
        let toml_str = crate_test_settings_str();
        Settings::from_toml(&toml_str).expect("Invalid config")
    }
}
