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

            <!-- Links with absolute origin URLs for attribute rewriting tests.
                 The trusted server should rewrite these from origin host to proxy host. -->
            <?php $origin = getenv('ORIGIN_HOST') ?: '127.0.0.1:8888'; ?>
            <a id="origin-link" href="http://<?= $origin ?>/page">Origin Link</a>
            <img id="origin-img" src="http://<?= $origin ?>/images/test.png" alt="test">

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
        <p>&copy; 2026 Test Publisher</p>
    </footer>
</body>
</html>
