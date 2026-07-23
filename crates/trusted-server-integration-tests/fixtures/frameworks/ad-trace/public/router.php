<?php
$path = parse_url($_SERVER['REQUEST_URI'], PHP_URL_PATH);

if ($path === '/health') {
    header('Content-Type: text/plain');
    echo 'ok';
    return;
}

if ($path === '/openrtb2/auction') {
    $request = json_decode(file_get_contents('php://input'), true) ?: [];
    $bids = [];
    foreach (($request['imp'] ?? []) as $index => $imp) {
        $slotId = is_string($imp['id'] ?? null) ? $imp['id'] : 'ad-trace-slot';
        $bids[] = [
            'id' => 'example-bid-' . ($index + 1),
            'impid' => $slotId,
            'adid' => 'example-ad-' . ($index + 1),
            'price' => 1.25,
            'adm' => '<div role="img" aria-label="Example creative">Example creative loaded</div>',
            'crid' => 'example-creative-' . ($index + 1),
            'w' => 300,
            'h' => 250,
            'adomain' => ['advertiser.example.com'],
        ];
    }

    header('Content-Type: application/json');
    echo json_encode([
        'id' => is_string($request['id'] ?? null) ? $request['id'] : 'example-auction',
        'seatbid' => $bids ? [['seat' => 'example-bidder', 'bid' => $bids]] : [],
        'cur' => 'USD',
    ], JSON_UNESCAPED_SLASHES);
    return;
}

if ($path === '/prebid-bundle.js') {
    header('Content-Type: application/javascript');
    readfile(__DIR__ . '/prebid-bundle.js');
    return;
}

if ($path === '/' || $path === '/spa-one' || $path === '/spa-two' || $path === '/publisher-only') {
    require __DIR__ . '/index.php';
    return;
}

http_response_code(404);
header('Content-Type: text/plain');
echo 'Not found';
