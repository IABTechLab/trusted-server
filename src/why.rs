pub const WHY_TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Why Trusted Server | Auburn DAO</title>
    <link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600&display=swap">
    <style>
        :root {
            --primary-text: #1A1A1A;
            --secondary-text: #6B7280;
            --link-color: #0066CC;
            --background: #FFFFFF;
            --border-color: #E5E7EB;
        }
        
        * {
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }
        
        body {
            font-family: 'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            line-height: 1.6;
            color: var(--primary-text);
            background: var(--background);
        }
        
        .container {
            max-width: 800px;
            margin: 0 auto;
            padding: 2rem 1.5rem;
        }
        
        nav {
            padding: 1.5rem 0;
            margin-bottom: 3rem;
        }
        
        .logo {
            font-size: 1.25rem;
            font-weight: 600;
            color: var(--primary-text);
            text-decoration: none;
        }
        
        h1 {
            font-size: 2.5rem;
            font-weight: 600;
            line-height: 1.2;
            margin-bottom: 2rem;
            letter-spacing: -0.02em;
        }
        
        h2 {
            font-size: 1.75rem;
            font-weight: 600;
            margin: 2.5rem 0 1.5rem;
            letter-spacing: -0.01em;
        }
        
        p {
            font-size: 1.125rem;
            margin-bottom: 1.5rem;
            color: var(--primary-text);
        }
        
        .subtitle {
            font-size: 1.25rem;
            color: var(--secondary-text);
            margin-bottom: 3rem;
            max-width: 44rem;
        }
        
        .feature-list {
            list-style: none;
            margin: 0;
            padding: 1rem 0;
        }
        
        .feature-list li {
            font-size: 1.125rem;
            margin-bottom: 1rem;
            padding-left: 1.5rem;
            position: relative;
        }
        
        .feature-list li::before {
            content: "•";
            position: absolute;
            left: 0;
            color: var(--link-color);
        }
        
        .section {
            margin: 3rem 0;
        }
        
        .content-card {
            background: var(--card-bg);
            border-radius: 16px;
            padding: 2rem;
            box-shadow: 
                0 4px 6px -1px rgba(0, 0, 0, 0.05),
                0 10px 15px -3px rgba(0, 0, 0, 0.1),
                0 -2px 4px -1px rgba(255, 255, 255, 0.5);
            position: relative;
            overflow: hidden;
            backdrop-filter: blur(5px);
            border: 1px solid rgba(255, 255, 255, 0.5);
        }
        
        .content-card::before {
            content: "";
            position: absolute;
            top: 0;
            left: 0;
            right: 0;
            bottom: 0;
            background: linear-gradient(135deg, var(--gradient-start), var(--gradient-end));
            opacity: 0.8;
            z-index: 0;
        }
        
        .content-card > * {
            position: relative;
            z-index: 1;
        }
        
        .emphasis {
            font-weight: 500;
            color: var(--link-color);
            position: relative;
            padding: 0 0.2em;
        }
        
        .emphasis::after {
            content: "";
            position: absolute;
            bottom: 0;
            left: 0;
            right: 0;
            height: 4px;
            background: currentColor;
            opacity: 0.1;
            border-radius: 2px;
        }
        
        a {
            color: var(--link-color);
            text-decoration: none;
        }
        
        a:hover {
            text-decoration: underline;
        }
        
        @media (max-width: 640px) {
            h1 {
                font-size: 2rem;
            }
            
            h2 {
                font-size: 1.5rem;
            }
            
            p, .feature-list li {
                font-size: 1rem;
            }
            
            .subtitle {
                font-size: 1.125rem;
            }
            
            .content-card {
                padding: 1.5rem;
                border-radius: 12px;
            }
        }
    </style>
</head>
<body>
    <div class="container">
        <nav>
            <a href="/" class="logo">Auburn DAO</a>
        </nav>
        
        <div class="content">
            <h1>Why Trusted Server</h1>
            
            <div class="section">
                <div class="content-card">
                    <p class="subtitle">Premium publishers have lost monetization capabilities due to big-tech browser decisions and reliance on 3rd party javascript. We feel that the ability to use 3rd party code and tags will continue its trend to zero and want to give publishers a privacy-first tool to fight back.</p>
                </div>
            </div>
            
            <div class="section">
                <h2>Our Solution</h2>
                <div class="content-card">
                    <p>We propose leveraging <span class="emphasis">first-party privileges</span> and <span class="emphasis">edge-cloud (server-side)</span> technology to help publishers take back control of advertising monetization and user-data security. We allow publishers to enable what is traditionally done via 3rd party code execution in a first party context. We have moved the ad stack out of the browser into a fast, secure edge-cloud environment.</p>
                </div>
            </div>
            
            <div class="section">
                <h2>Key Features</h2>
                <div class="content-card">
                    <ul class="feature-list">
                        <li>Manage Ad Request and Ad Response</li>
                        <li>Server Side Ad Stitching</li>
                        <li>Prebid server integration</li>
                        <li>Edge Cloud initiation and data signals collection</li>
                        <li>Plugin support for 3P providers for identity and audience, fraud prevention, brand safety</li>
                        <li>Plug and play into existing programmatic systems (minimal changes)</li>
                    </ul>
                </div>
            </div>
        </div>
    </div>
</body>
</html>"#;
