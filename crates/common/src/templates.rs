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
                location.reload();
            });
        }

        // Load ads and tracking based on consent
        window.addEventListener('load', function() {
            showGdprBanner();
            
            // Get consent status
            const consent = getCookie('gdpr_consent');
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
