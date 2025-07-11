#[cfg(test)]
pub mod tests {
    use crate::settings::{AdServer, Gam, GamAdUnit, Prebid, Publisher, Settings, Synthetic};

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
        Settings {
            ad_server: AdServer {
                ad_partner_url: "https://test-adpartner.com".into(),
                sync_url: "https://test-adpartner.com/synthetic_id={{synthetic_id}}".to_string(),
            },
            publisher: Publisher {
                domain: "test-publisher.com".to_string(),
                cookie_domain: ".test-publisher.com".to_string(),
                origin_url: "origin.test-publisher.com".to_string(),
            },
            prebid: Prebid {
                server_url: "https://test-prebid.com/openrtb2/auction".to_string(),
            },
            gam: Gam {
                publisher_id: "test-publisher-id".to_string(),
                server_url: "https://securepubads.g.doubleclick.net/gampad/ads".to_string(),
                ad_units: vec![GamAdUnit { name: "test-ad-unit".to_string(), size: "300x250".to_string() }],
            },
            synthetic: Synthetic {
                counter_store: "test_counter_store".to_string(),
                opid_store: "test-opid-store".to_string(),
                secret_key: "test-secret-key".to_string(),
                template: "{{client_ip}}:{{user_agent}}:{{first_party_id}}:{{auth_user_id}}:{{publisher_domain}}:{{accept_language}}".to_string(),
            },
        }
    }
}
