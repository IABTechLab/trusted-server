const { creative } = require("../initial-render/pages.cjs");

function inlineJson(value) {
  return JSON.stringify(value)
    .replaceAll("<", "\\u003c")
    .replaceAll(">", "\\u003e")
    .replaceAll("&", "\\u0026")
    .replaceAll("\u2028", "\\u2028")
    .replaceAll("\u2029", "\\u2029");
}

function pucPage(publisherOrigin = "https://publisher.example.com") {
  return `<!doctype html><script src="/fixture-puc.js"></script><script>
window.ucTag.renderAd(document, {
  adId: new URL(window.location.href).searchParams.get("adId"),
  pubUrl: ${inlineJson(publisherOrigin)},
});
</script>`;
}

// Direct-payload offline cases only. Framework proof must use Rust-injected data.
function payload({ registered = true, two = false, candidate = "fit" } = {}) {
  const slots = (two ? ["header", "sidebar"] : ["header"]).map((id) => ({
    id,
    gam_unit_path: "/123/example-placement",
    div_id: "example-" + id + "-",
    formats: [
      [300, 250],
      [300, 600],
    ],
    ...(registered ? { initialization: "publisher_registered" } : {}),
    targeting: { example_section: ["fiction"] },
  }));
  const bids = Object.fromEntries(
    slots.map((slot) => [
      slot.id,
      candidate === "none"
        ? {}
        : {
            hb_adid: "example-ad-" + slot.id,
            hb_bidder: "exampleServer",
            hb_pb: "1.00",
            ...(candidate !== "unknown"
              ? { w: 300, h: candidate === "incompatible" ? 600 : 250 }
              : {}),
            adm: creative("Server result"),
            nurl:
              "https://publisher.example.com/fixture-notice/win?slot=" +
              slot.id,
            burl:
              "https://publisher.example.com/fixture-notice/bill?slot=" +
              slot.id,
          },
    ]),
  );
  return { slots, bids };
}

function documentPage(data = {}) {
  return `<!doctype html><title>Publisher registration contract</title>
<h1>Publisher registration contract</h1><div id="mount"></div>
<script>window.publisherFixtureOptions = ${inlineJson(data)};</script>`;
}

module.exports = { inlineJson, payload, documentPage, creative, pucPage };
