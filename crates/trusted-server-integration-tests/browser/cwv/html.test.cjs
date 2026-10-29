const { test } = require("node:test");
const assert = require("node:assert/strict");
const {
  reservationStyles,
  scriptBlocks,
  withoutScripts,
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
    withoutEarlyRenderer(
      raw +
        "<SCRIPT ID=trustedserver-bootstrap>installRealBridge();</SCRIPT>" +
        runtime,
    ),
    raw + runtime,
  );
  for (const name of ["gpt", "unified"]) {
    assert(rendererAsset(`/static/tsjs=tsjs-${name}.min.js`));
    assert(rendererAsset(`/static/tsjs=tsjs-${name}.js`));
  }
  assert(!rendererAsset("/static/tsjs=tsjs-prebid.min.js"));
});

test("script blocks preserve raw content, source offsets and decoded attributes", () => {
  const prefix = "<!doctype html>\r\n<!-- 😀 -->";
  const tag =
    '<SCRIPT ID=probe SRC="/runtime.js?x=1&amp;y=2" data-note="a>b">\r\nconst value = "<&amp;";\r\n</SCRIPT >';
  const html = prefix + tag + "\r\n<p>unchanged</p>";
  const [block] = scriptBlocks(html);
  assert.deepEqual(block, {
    id: "probe",
    src: "/runtime.js?x=1&y=2",
    content: '\r\nconst value = "<&amp;";\r\n',
    startOffset: prefix.length,
    endOffset: prefix.length + tag.length,
  });
  assert.equal(html.slice(block.startOffset, block.endOffset), tag);
  assert.equal(
    withoutScripts(html, (script) => script.id === "probe"),
    prefix + "\r\n<p>unchanged</p>",
  );
});

test("renderer removal accepts HTML tag syntax while matching ID values exactly", () => {
  const keep =
    '<script id="TRUSTEDSERVER-BOOTSTRAP">keepCaseSensitiveId();</script>';
  for (const close of ["</script >", "</script/>", "</SCRIPT \t>"]) {
    const selected =
      '<SCRIPT data-note=">" ID=trustedserver-bootstrap>remove();' + close;
    const html = "prefix\r\n" + keep + selected + "<p>suffix &amp;</p>";
    assert.equal(
      withoutEarlyRenderer(html),
      "prefix\r\n" + keep + "<p>suffix &amp;</p>",
    );
  }
});

test("script-looking comments, strings, attributes and inert elements stay byte-identical", () => {
  const html =
    '<div data-text="<script id=trustedserver-bootstrap>"></div>' +
    "<!-- <script id=trustedserver-bootstrap>comment</script> -->" +
    "<textarea><script id=trustedserver-bootstrap>text</script></textarea>" +
    "<template><script id=trustedserver-bootstrap>template</script></template>" +
    '<script id=other>const text = "<script id=trustedserver-bootstrap>";</script>';
  assert.deepEqual(
    scriptBlocks(html).map((script) => script.id),
    ["other"],
  );
  assert.equal(withoutEarlyRenderer(html), html);
});

test("probe and runtime removal preserve every unselected script byte", () => {
  const before = '<script>const text = "window.fixtureEarly=";</script>\r\n';
  const probe =
    '<SCRIPT data-note=">" > \r\n window.fixtureEarly={core:true};</SCRIPT/>';
  const runtime = "<SCRIPT SRC=/runtime.js ID=trustedserver-js> </SCRIPT >";
  const after = "<script id=last>keep();</script>";
  const html = before + probe + runtime + after;
  assert.equal(
    withoutScripts(html, (script) =>
      script.content.trimStart().startsWith("window.fixtureEarly="),
    ),
    before + runtime + after,
  );
  assert.equal(
    withoutScripts(html, (script) => script.id === "trustedserver-js"),
    before + probe + after,
  );
});

test("an unclosed HTML script extends to EOF when jsdom leaves its end bound unset", () => {
  for (const prefix of ["", "<p>before</p>"]) {
    const content = "const value = 'open';";
    const html = prefix + "<script id=a>" + content;
    const [script] = scriptBlocks(html);
    assert.equal(script.content, content);
    assert.equal(script.endOffset, html.length);
    assert.equal(
      withoutScripts(html, (block) => block.id === "a"),
      prefix,
    );
  }
});

test("a self-closing SVG script uses its valid end offset without consuming following bytes", () => {
  const html = '<svg><script id="a"/></svg><p>after</p><script id=b>B</script>';
  const [script] = scriptBlocks(html);
  assert.equal(script.content, "");
  assert.equal(script.endOffset, html.indexOf("</svg>"));
  assert.equal(
    withoutScripts(html, (block) => block.id === "a"),
    "<svg></svg><p>after</p><script id=b>B</script>",
  );
  assert.equal(
    withoutScripts(html, () => true),
    "<svg></svg><p>after</p>",
  );
});

test("table foster parenting does not reorder physical source splices", () => {
  const html =
    "<table><script id=a>A</script><div><script id=b>B</script></div></table>";
  assert.deepEqual(
    scriptBlocks(html).map((script) => script.id),
    ["a", "b"],
  );
  assert.equal(
    withoutScripts(html, () => true),
    "<table><div></div></table>",
  );
});

test("overlapping actual SVG script ranges are removed once without duplicating other bytes", () => {
  const nested =
    "<svg><script id=outer><script id=inner>value</script></script></svg>";
  assert.equal(
    withoutScripts(nested + "<p>keep</p>", () => true),
    "<svg></svg><p>keep</p>",
  );
  assert.equal(
    withoutScripts(nested, (script) => script.id === "inner"),
    "<svg><script id=outer></script></svg>",
  );
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
