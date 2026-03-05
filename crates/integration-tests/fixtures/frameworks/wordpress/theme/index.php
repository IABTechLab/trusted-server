<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Test Publisher - WordPress</title>
    <link rel="stylesheet" href="/wp-content/themes/test/style.css">
</head>
<body>
    <header>
        <h1>Integration Test Publisher</h1>
        <nav>
            <a href="/">Home</a>
            <a href="/about">About</a>
        </nav>
    </header>

    <main>
        <article>
            <h2>Test Article</h2>
            <p>This is a test article for integration testing of the trusted server.</p>

            <!-- Ad slot that should be rewritten by the trusted server -->
            <div id="ad-slot-1" data-ad-unit="/test/banner">
                <p>Advertisement placeholder</p>
            </div>

            <p>More article content follows the ad slot.</p>

            <!-- Second ad slot -->
            <div id="ad-slot-2" data-ad-unit="/test/sidebar">
                <p>Sidebar advertisement placeholder</p>
            </div>
        </article>
    </main>

    <footer>
        <p>&copy; 2025 Test Publisher</p>
    </footer>
</body>
</html>
