const { JSDOM } = require("jsdom");

// Keep the exact Rust-emitted reservation tag, not iframe-template style strings
// inside the inline production bundle and not a DOM reserialization.
function reservationStyles(html) {
  return (
    html.match(
      /<style\b[^>]*\bdata-ts-slot-reservations\b[^>]*>[\s\S]*?<\/style>/gi,
    ) ?? []
  );
}

function scriptBlocks(html) {
  const dom = new JSDOM(html, { includeNodeLocations: true });
  try {
    return [...dom.window.document.querySelectorAll("script")]
      .map((script) => {
        const location = dom.nodeLocation(script);
        const endOffset =
          location.endOffset >= location.startTag.endOffset
            ? location.endOffset
            : html.length;
        return {
          content: html.slice(
            location.startTag.endOffset,
            location.endTag?.startOffset ?? endOffset,
          ),
          id: script.getAttribute("id") ?? "",
          src: script.getAttribute("src"),
          startOffset: location.startOffset,
          endOffset,
        };
      })
      .sort((left, right) => left.startOffset - right.startOffset);
  } finally {
    dom.window.close();
  }
}

function withoutScripts(html, predicate) {
  const pieces = [];
  let offset = 0;
  for (const script of scriptBlocks(html)) {
    if (!predicate(script)) continue;
    if (script.startOffset > offset)
      pieces.push(html.slice(offset, script.startOffset));
    offset = Math.max(offset, script.endOffset);
  }
  pieces.push(html.slice(offset));
  return pieces.join("");
}

function withoutEarlyRenderer(html) {
  return withoutScripts(
    html,
    (script) => script.id === "trustedserver-bootstrap",
  );
}

function rendererAsset(pathname) {
  return /^\/static\/tsjs=tsjs-(?:gpt|unified)(?:\.min)?\.js$/.test(pathname);
}

module.exports = {
  reservationStyles,
  scriptBlocks,
  withoutScripts,
  withoutEarlyRenderer,
  rendererAsset,
};
