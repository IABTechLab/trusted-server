use std::collections::HashMap;

pub const HTML_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Travel Southeast Asia</title>
    <style>
        body {
            font-family: Arial, sans-serif;
            margin: 0;
            padding: 0;
            background-color: #f4f4f4;
        }
        header {
            background: url('https://picsum.photos/1200/400?random=1') no-repeat center center;
            background-size: cover;
            color: white;
            text-align: center;
            padding: 60px 20px;
        }
        header h1 {
            font-size: 3em;
            margin: 0;
        }
        main {
            display: flex;
            flex-wrap: wrap;
            justify-content: center;
            padding: 20px;
        }
        .location {
            background: white;
            border-radius: 8px;
            box-shadow: 0 4px 8px rgba(0,0,0,0.1);
            margin: 15px;
            overflow: hidden;
            width: 300px;
            transition: transform 0.3s;
        }
        .location:hover {
            transform: translateY(-10px);
        }
        .location img {
            width: 100%;
            height: 200px;
            object-fit: cover;
        }
        .location h2 {
            font-size: 1.5em;
            margin: 15px;
        }
        .location p {
            margin: 0 15px 15px;
            color: #555;
        }
        .ad-container {
            width: 100%;
            text-align: center;
            margin: 30px 0;
        }
        
        /* GDPR Consent Banner */
        #gdpr-banner {
            position: fixed;
            bottom: 0;
            left: 0;
            right: 0;
            background: rgba(0, 0, 0, 0.9);
            color: white;
            padding: 20px;
            z-index: 1000;
            display: none;
        }
        #gdpr-banner.visible {
            display: block;
        }
        .gdpr-buttons {
            margin-top: 10px;
        }
        .gdpr-buttons button {
            margin: 5px;
            padding: 8px 16px;
            border: none;
            border-radius: 4px;
            cursor: pointer;
        }
        .gdpr-accept {
            background: #4CAF50;
            color: white;
        }
        .gdpr-customize {
            background: #2196F3;
            color: white;
        }
        .gdpr-reject {
            background: #f44336;
            color: white;
        }
        #gdpr-preferences {
            display: none;
            position: fixed;
            top: 50%;
            left: 50%;
            transform: translate(-50%, -50%);
            background: white;
            padding: 20px;
            border-radius: 8px;
            box-shadow: 0 0 20px rgba(0,0,0,0.2);
            z-index: 1001;
        }
        #gdpr-preferences.visible {
            display: block;
        }
        .preference-item {
            margin: 10px 0;
        }
        .overlay {
            display: none;
            position: fixed;
            top: 0;
            left: 0;
            right: 0;
            bottom: 0;
            background: rgba(0,0,0,0.5);
            z-index: 999;
        }
        .overlay.visible {
            display: block;
        }
    </style>
    <script>
        // GDPR Consent Management
        function showGdprBanner() {
            const consent = getCookie('gdpr_consent');
            if (!consent) {
                document.getElementById('gdpr-banner').classList.add('visible');
                document.querySelector('.overlay').classList.add('visible');
            }
        }

        function getCookie(name) {
            const value = `; ${document.cookie}`;
            const parts = value.split(`; ${name}=`);
            if (parts.length === 2) return parts.pop().split(';').shift();
        }

        function handleConsent(type) {
            if (type === 'customize') {
                document.getElementById('gdpr-preferences').classList.add('visible');
                return;
            }

            const consent = {
                analytics: type === 'accept',
                advertising: type === 'accept',
                functional: type === 'accept',
                timestamp: Date.now(),
                version: "1.0"
            };

            saveConsent(consent);
        }

        function savePreferences() {
            const consent = {
                analytics: document.getElementById('analytics-consent').checked,
                advertising: document.getElementById('advertising-consent').checked,
                functional: document.getElementById('functional-consent').checked,
                timestamp: Date.now(),
                version: "1.0"
            };

            saveConsent(consent);
        }

        function saveConsent(consent) {
            // Set the cookie first
            document.cookie = `gdpr_consent=${JSON.stringify(consent)}; path=/; max-age=31536000`; // 1 year expiry
            
            fetch('/gdpr/consent', {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                },
                body: JSON.stringify(consent)
            }).then(() => {
                document.getElementById('gdpr-banner').classList.remove('visible');
                document.getElementById('gdpr-preferences').classList.remove('visible');
                document.querySelector('.overlay').classList.remove('visible');
                // Remove the reload - we'll let the page continue with the new consent
            }).catch(error => {
                console.error('Error saving consent:', error);
            });
        }

        // Load ads and tracking based on consent
        window.addEventListener('load', function() {
            const consent = getCookie('gdpr_consent');
            if (!consent) {
                showGdprBanner();
            }
            
            // Get consent status
            const consentData = consent ? JSON.parse(consent) : { advertising: false, functional: false };

            // Always make the prebid request, but include consent information
            fetch('/prebid-test', {
                headers: {
                    'X-Consent-Advertising': consentData.advertising ? 'true' : 'false',
                    'X-Consent-Functional': consentData.functional ? 'true' : 'false'
                }
            })
            .then(response => response.json())
            .then(data => {
                console.log('Prebid response:', data);
                // Here you can use the prebid response data
            })
            .catch(error => console.error('Prebid error:', error));

            // Always fetch ad creative, but it will be non-personalized if no consent
            fetch('/ad-creative', {
                headers: {
                    'X-Consent-Advertising': consentData.advertising ? 'true' : 'false',
                    'X-Consent-Functional': consentData.functional ? 'true' : 'false'
                }
            })
            .then(response => response.json())
            .then(data => {
                console.log('Ad response:', data);
                if (data && data.creativeUrl) {
                    const adContainer = document.getElementById('ad-container');
                    const adLink = document.createElement('a');
                    adLink.href = 'https://iabtechlab.com/?potsi-test%3F';
                    const adImage = document.createElement('img');
                    adImage.src = data.creativeUrl.replace('creatives.sascdn.com', 'creatives.auburndao.com');
                    adImage.alt = 'Ad Creative';
                    adLink.appendChild(adImage);
                    adContainer.appendChild(adLink);
                }
            })
            .catch(error => {
                console.error('Error:', error);
                // Optionally hide the ad container on error
                document.getElementById('ad-container').style.display = 'none';
            });
        });
    </script>
</head>
<body>
    <!-- GDPR Consent Banner -->
    <div class="overlay"></div>
    <div id="gdpr-banner">
        <h2>Cookie Consent</h2>
        <p>We use cookies to enhance your browsing experience, serve personalized ads or content, and analyze our traffic. By clicking "Accept All", you consent to our use of cookies.</p>
        <div class="gdpr-buttons">
            <button class="gdpr-accept" onclick="handleConsent('accept')">Accept All</button>
            <button class="gdpr-customize" onclick="handleConsent('customize')">Customize</button>
            <button class="gdpr-reject" onclick="handleConsent('reject')">Reject All</button>
        </div>
        <p><small>For more information, please read our <a href="/privacy-policy" style="color: white;">Privacy Policy</a></small></p>
    </div>

    <!-- GDPR Preferences Modal -->
    <div id="gdpr-preferences">
        <h2>Cookie Preferences</h2>
        <div class="preference-item">
            <input type="checkbox" id="functional-consent">
            <label for="functional-consent">Functional Cookies</label>
            <p><small>Essential for the website to function properly. Cannot be disabled.</small></p>
        </div>
        <div class="preference-item">
            <input type="checkbox" id="analytics-consent">
            <label for="analytics-consent">Analytics Cookies</label>
            <p><small>Help us understand how visitors interact with our website.</small></p>
        </div>
        <div class="preference-item">
            <input type="checkbox" id="advertising-consent">
            <label for="advertising-consent">Advertising Cookies</label>
            <p><small>Used to provide you with personalized advertising.</small></p>
        </div>
        <div class="gdpr-buttons">
            <button class="gdpr-accept" onclick="savePreferences()">Save Preferences</button>
        </div>
    </div>

    <header>
        <h1>Explore the Wonders of Southeast Asia</h1>
    </header>

    <main>
        <div class="location">
            <img src="https://picsum.photos/300/200?random=2" alt="Thailand">
            <h2>Thailand</h2>
            <p>Experience the vibrant culture and stunning beaches of Thailand.</p>
        </div>
        <div class="location">
            <img src="https://picsum.photos/300/200?random=3" alt="Vietnam">
            <h2>Vietnam</h2>
            <p>Discover the rich history and breathtaking landscapes of Vietnam.</p>
        </div>
        <div class="location">
            <img src="https://picsum.photos/300/200?random=4" alt="Indonesia">
            <h2>Indonesia</h2>
            <p>Explore the diverse islands and unique traditions of Indonesia.</p>
        </div>
        <div class="location">
            <img src="https://picsum.photos/300/200?random=5" alt="Malaysia">
            <h2>Malaysia</h2>
            <p>Enjoy the blend of modernity and nature in Malaysia.</p>
        </div>
    </main>

    <!-- Advertisement Section -->
    <!-- Comment out old version
    <div class="ad-container">
        <a href="https://iabtechlab.com/?potsi-test%3F">
            <img src="{CREATIVE_URL}" alt="Ad Creative">
        </a>
    </div>
    -->

    <!-- New async version -->
    <div id="ad-container" class="ad-container">
        <!-- Ad will be loaded here -->
    </div>

</body>
</html>"#;

pub const GAM_TEST_TEMPLATE: &str = r#"
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>GAM Test - Trusted Server</title>
    <style>
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            max-width: 1200px;
            margin: 0 auto;
            padding: 20px;
            background-color: #f5f5f5;
        }
        .container {
            background: white;
            padding: 30px;
            border-radius: 8px;
            box-shadow: 0 2px 10px rgba(0,0,0,0.1);
        }
        h1 {
            color: #333;
            border-bottom: 2px solid #007cba;
            padding-bottom: 10px;
        }
        .phase {
            background: #f8f9fa;
            border-left: 4px solid #007cba;
            padding: 15px;
            margin: 20px 0;
            border-radius: 4px;
        }
        .phase h3 {
            margin-top: 0;
            color: #007cba;
        }
        .test-section {
            margin: 20px 0;
            padding: 20px;
            border: 1px solid #ddd;
            border-radius: 4px;
        }
        button {
            background: #007cba;
            color: white;
            border: none;
            padding: 10px 20px;
            border-radius: 4px;
            cursor: pointer;
            margin: 5px;
        }
        button:hover {
            background: #005a87;
        }
        button:disabled {
            background: #ccc;
            cursor: not-allowed;
        }
        .result {
            background: #f8f9fa;
            border: 1px solid #ddd;
            border-radius: 4px;
            padding: 15px;
            margin: 10px 0;
            white-space: pre-wrap;
            font-family: monospace;
            max-height: 400px;
            overflow-y: auto;
        }
        .status {
            padding: 10px;
            border-radius: 4px;
            margin: 10px 0;
        }
        .status.success {
            background: #d4edda;
            color: #155724;
            border: 1px solid #c3e6cb;
        }
        .status.error {
            background: #f8d7da;
            color: #721c24;
            border: 1px solid #f5c6cb;
        }
        .status.info {
            background: #d1ecf1;
            color: #0c5460;
            border: 1px solid #bee5eb;
        }
        .instructions {
            background: #fff3cd;
            border: 1px solid #ffeaa7;
            border-radius: 4px;
            padding: 15px;
            margin: 20px 0;
        }
        .instructions h4 {
            margin-top: 0;
            color: #856404;
        }
    </style>
</head>
<body>
    <div class="container">
        <h1>GAM Test - Headless GPT PoC</h1>
        
        <div class="instructions">
            <h4>📋 Instructions for Capture & Replay Phase</h4>
            <p><strong>Phase 1 Goal:</strong> Capture a complete, successful ad request URL from autoblog.com and replay it from our server.</p>
            <ol>
                <li>Open browser dev tools on autoblog.com</li>
                <li>Go to Network tab and filter by "g.doubleclick.net"</li>
                <li>Refresh the page and look for successful ad requests</li>
                <li>Copy the complete URL with all parameters</li>
                <li>Use the "Test Golden URL" button below to test it</li>
            </ol>
        </div>

        <div class="phase">
            <h3>Phase 1: Capture & Replay (Golden URL)</h3>
            <p>Test the exact captured URL from autoblog.com to prove network connectivity.</p>
            
            <div class="test-section">
                <h4>Golden URL Test</h4>
                <p>Paste the captured GAM URL from autoblog.com below and test it:</p>
                <div style="margin: 15px 0;">
                    <textarea 
                        id="goldenUrlInput" 
                        placeholder="Paste the captured GAM URL here (e.g., https://securepubads.g.doubleclick.net/gampad/ads?pvsid=...)"
                        style="width: 100%; height: 100px; font-family: monospace; font-size: 12px; padding: 10px; border: 1px solid #ddd; border-radius: 4px;"
                    ></textarea>
                </div>
                <button onclick="testGoldenUrl()">Test Golden URL</button>
                <button onclick="testBuiltInGoldenUrl()">Test Built-in Template</button>
                <div id="goldenUrlResult" class="result" style="display: none;"></div>
            </div>
        </div>

        <div class="phase">
            <h3>Phase 2: Dynamic Request Building</h3>
            <p>Test dynamic parameter generation with hardcoded prmtvctx value.</p>
            
            <div class="test-section">
                <h4>Dynamic GAM Request</h4>
                <p>Test server-side GAM request with dynamic correlator and synthetic ID.</p>
                <button onclick="testDynamicGam()">Test Dynamic GAM Request</button>
                <div id="dynamicGamResult" class="result" style="display: none;"></div>
            </div>
        </div>

        <div class="phase">
            <h3>Phase 3: Ad Rendering in iFrame</h3>
            <p>Render the GAM response HTML content in a sandboxed iframe for visual testing.</p>
            
            <div class="test-section">
                <h4>Ad Render Test</h4>
                <p>Test rendering the GAM response as an actual ad in an iframe:</p>
                <button onclick="testAdRender()">🎯 Render Ad in iFrame</button>
                <button onclick="window.open('/gam-render', '_blank')">🔄 Open Render Page</button>
                <div id="renderResult" class="status info" style="display: none;">
                    Opening ad render page in new tab...
                </div>
            </div>
        </div>

        <div class="phase">
            <h3>Debug Information</h3>
            <div class="test-section">
                <h4>Request Headers</h4>
                <div id="headers" class="result"></div>
                
                <h4>Synthetic ID Status</h4>
                <div id="syntheticStatus" class="status info">
                    Checking synthetic ID...
                </div>
            </div>
        </div>
    </div>

    <script>
        // Display request headers for debugging
        function displayHeaders() {
            const headers = {};
            // Note: We can't access all headers from client-side, but we can show what we know
            headers['User-Agent'] = navigator.userAgent;
            headers['Accept'] = 'application/json, text/plain, */*';
            headers['Accept-Language'] = navigator.language;
            
            document.getElementById('headers').textContent = JSON.stringify(headers, null, 2);
        }

        // Check synthetic ID status
        async function checkSyntheticId() {
            try {
                const response = await fetch('/');
                const freshId = response.headers.get('X-Synthetic-Fresh');
                const trustedServerId = response.headers.get('X-Synthetic-Trusted-Server');
                
                const statusDiv = document.getElementById('syntheticStatus');
                statusDiv.className = 'status success';
                statusDiv.innerHTML = `
                    <strong>Synthetic IDs:</strong><br>
                    Fresh ID: ${freshId || 'Not found'}<br>
                    Trusted Server ID: ${trustedServerId || 'Not found'}
                `;
            } catch (error) {
                document.getElementById('syntheticStatus').className = 'status error';
                document.getElementById('syntheticStatus').textContent = 'Error checking synthetic ID: ' + error.message;
            }
        }

        // Test Golden URL replay
        async function testGoldenUrl() {
            const resultDiv = document.getElementById('goldenUrlResult');
            const urlInput = document.getElementById('goldenUrlInput');
            resultDiv.style.display = 'block';
            
            const customUrl = urlInput.value.trim();
            if (!customUrl) {
                resultDiv.textContent = 'Error: Please paste a GAM URL in the textarea above.';
                return;
            }
            
            resultDiv.textContent = 'Testing Custom Golden URL...';
            
            try {
                const response = await fetch('/gam-test-custom-url', {
                    method: 'POST',
                    headers: {
                        'Content-Type': 'application/json',
                        'X-Consent-Advertising': 'true'
                    },
                    body: JSON.stringify({ url: customUrl })
                });
                
                const data = await response.json();
                resultDiv.textContent = JSON.stringify(data, null, 2);
            } catch (error) {
                resultDiv.textContent = 'Error: ' + error.message;
            }
        }

        // Test built-in Golden URL template
        async function testBuiltInGoldenUrl() {
            const resultDiv = document.getElementById('goldenUrlResult');
            resultDiv.style.display = 'block';
            resultDiv.textContent = 'Testing Built-in Golden URL Template...';
            
            try {
                const response = await fetch('/gam-golden-url');
                const data = await response.json();
                
                resultDiv.textContent = JSON.stringify(data, null, 2);
            } catch (error) {
                resultDiv.textContent = 'Error: ' + error.message;
            }
        }

        // Test dynamic GAM request
        async function testDynamicGam() {
            const resultDiv = document.getElementById('dynamicGamResult');
            resultDiv.style.display = 'block';
            resultDiv.textContent = 'Testing Dynamic GAM Request...';
            
            try {
                // First get the main page to ensure we have synthetic IDs
                const mainResponse = await fetch('/');
                const freshId = mainResponse.headers.get('X-Synthetic-Fresh');
                const trustedServerId = mainResponse.headers.get('X-Synthetic-Trusted-Server');
                
                // Now test the GAM request
                const response = await fetch('/gam-test', {
                    headers: {
                        'X-Consent-Advertising': 'true',
                        'X-Synthetic-Fresh': freshId || '',
                        'X-Synthetic-Trusted-Server': trustedServerId || ''
                    }
                });
                
                // Get the response as text first (since it contains both JSON and HTML)
                const responseText = await response.text();
                
                // Try to parse as JSON first (in case it's a pure JSON response)
                let data;
                try {
                    data = JSON.parse(responseText);
                } catch (jsonError) {
                    // If JSON parsing fails, it's likely the mixed JSON+HTML format
                    // Find the end of the JSON part (before the HTML starts)
                    const htmlStart = responseText.indexOf('<!doctype html>');
                    if (htmlStart !== -1) {
                        // Extract just the JSON part
                        const jsonPart = responseText.substring(0, htmlStart);
                        try {
                            data = JSON.parse(jsonPart);
                            // Add info about the HTML part
                            data.html_content_length = responseText.length - htmlStart;
                            data.full_response_length = responseText.length;
                        } catch (innerError) {
                            // If we still can't parse JSON, show the raw response
                            data = {
                                error: 'Could not parse GAM response as JSON',
                                raw_response_preview: responseText.substring(0, 500) + '...',
                                response_length: responseText.length
                            };
                        }
                    } else {
                        // No HTML found, show the raw response
                        data = {
                            error: 'Unexpected response format',
                            raw_response: responseText,
                            response_length: responseText.length
                        };
                    }
                }
                
                resultDiv.textContent = JSON.stringify(data, null, 2);
            } catch (error) {
                resultDiv.textContent = 'Error: ' + error.message;
            }
        }

        // Test ad rendering in iframe
        async function testAdRender() {
            const resultDiv = document.getElementById('renderResult');
            resultDiv.style.display = 'block';
            resultDiv.textContent = 'Opening ad render page in new tab...';
            
            // Open the render page in a new tab
            window.open('/gam-render', '_blank');
            
            // Update the result message
            setTimeout(() => {
                resultDiv.textContent = 'Ad render page opened in new tab. Check the new tab to see the rendered ad!';
                resultDiv.className = 'status success';
            }, 1000);
        }

        // Initialize page
        document.addEventListener('DOMContentLoaded', function() {
            displayHeaders();
            checkSyntheticId();
        });
    </script>
</body>
</html>
"#;
// GAM Configuration Template
#[allow(dead_code)]
struct GamConfigTemplate {
    publisher_id: String,
    ad_units: Vec<AdUnitConfig>,
    page_context: PageContext,
    data_providers: Vec<DataProvider>,
}
#[allow(dead_code)]
struct AdUnitConfig {
    name: String,
    sizes: Vec<String>,
    position: String,
    targeting: HashMap<String, String>,
}
#[allow(dead_code)]
struct PageContext {
    page_type: String,
    section: String,
    keywords: Vec<String>,
}
#[allow(dead_code)]
enum DataProvider {
    Permutive(PermutiveConfig),
    Lotame(LotameConfig),
    Neustar(NeustarConfig),
    Custom(CustomProviderConfig),
}
#[allow(dead_code)]
struct PermutiveConfig {}
#[allow(dead_code)]
struct LotameConfig {}
#[allow(dead_code)]
struct NeustarConfig {}
#[allow(dead_code)]
struct CustomProviderConfig {}
#[allow(dead_code)]
trait DataProviderTrait {
    fn get_user_segments(&self, user_id: &str) -> Vec<String>;
}

#[allow(dead_code)]
struct RequestContext {
    user_id: String,
    page_url: String,
    consent_status: bool,
}

#[allow(dead_code)]
struct DynamicGamBuilder {
    base_config: GamConfigTemplate,
    context: RequestContext,
    data_providers: Vec<Box<dyn DataProviderTrait>>,
}

// Instead of hardcoded strings, use templates:
// "cust_params": "{{#each data_providers}}{{name}}={{segments}}&{{/each}}puid={{user_id}}"

// This could generate:
// "permutive=129627,137412...&lotame=segment1,segment2&puid=abc123"

// let context = data_provider_manager.build_context(&user_id, &request_context);
// let gam_req_with_context = gam_req.with_dynamic_context(context);
