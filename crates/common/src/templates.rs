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
    </style>
    <script>
        // Make the prebid request when the page loads
        window.addEventListener('load', function() {
            fetch('/prebid-test')
                .then(response => response.json())
                .then(data => {
                    console.log('Prebid response:', data);
                    // Here you can use the prebid response data
                })
                .catch(error => console.error('Prebid error:', error));
        });

        // Existing ad request code
        window.addEventListener('load', function() {
            fetch('/ad-creative')
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
