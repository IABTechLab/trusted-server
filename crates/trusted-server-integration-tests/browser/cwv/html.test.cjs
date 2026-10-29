const { test } = require("node:test");
const assert = require("node:assert/strict");
const {
  reservationStyles,
  withoutEarlyRenderer,
  rendererAsset,
} = require("./html.cjs");

test("renderer absence removes compiled early code but retains raw protocol bootstrap", () => {
  const raw =
    "<script>window.tsjs={publisherSlotRegistrationVersion:1};</script>";
  const runtime =
    '<script src="/static/tsjs=tsjs-gpt.min.js" id="trustedserver-js" defer></script>';
  const early =
    '<script id="trustedserver-bootstrap">installRealBridge();</script>';
  assert.equal(withoutEarlyRenderer(raw + early + runtime), raw + runtime);
  assert.equal(
    withoutEarlyRenderer(raw + early.toUpperCase() + runtime),
    raw + runtime,
  );
  for (const name of ["gpt", "unified"]) {
    assert(rendererAsset(`/static/tsjs=tsjs-${name}.min.js`));
    assert(rendererAsset(`/static/tsjs=tsjs-${name}.js`));
  }
  assert(!rendererAsset("/static/tsjs=tsjs-prebid.min.js"));
});

test("CSS-only control copies only the exact production reservation stylesheet", () => {
  const css =
    '<style data-ts-slot-reservations>[id="space"]{min-height:250px}</style>';
  const html =
    "<head>" +
    css +
    '<script>const iframe="<style>%NORMALIZE_CSS%</style>";</script><style>body{margin:0}</style></head>';
  assert.deepEqual(reservationStyles(html), [css]);
  assert.deepEqual(reservationStyles("<style>body{margin:0}</style>"), []);
});
