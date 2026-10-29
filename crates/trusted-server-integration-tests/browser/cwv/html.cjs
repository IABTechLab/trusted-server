// Keep the exact Rust-emitted reservation tag, not iframe-template style strings
// inside the inline production bundle and not a DOM reserialization.
function reservationStyles(html) {
  return (
    html.match(
      /<style\b[^>]*\bdata-ts-slot-reservations\b[^>]*>[\s\S]*?<\/style>/gi,
    ) ?? []
  );
}

function withoutEarlyRenderer(html) {
  return html.replace(
    /<script\b[^>]*\bid=["']trustedserver-bootstrap["'][^>]*>[\s\S]*?<\/script>/gi,
    "",
  );
}

function rendererAsset(pathname) {
  return /^\/static\/tsjs=tsjs-(?:gpt|unified)(?:\.min)?\.js$/.test(pathname);
}

module.exports = { reservationStyles, withoutEarlyRenderer, rendererAsset };
