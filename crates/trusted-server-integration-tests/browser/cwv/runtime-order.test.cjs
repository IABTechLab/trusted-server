const { test } = require("node:test");
const assert = require("node:assert/strict");
const { assertRuntimeOrder } = require("./runtime-order.cjs");

const origin = "https://publisher.example.com";
const configs = [
  "window.__tsjs_gpt_enabled=true",
  "window.__tsjs_prebid={}",
].map((content) => `<script>${content}</script>`);
const runtime =
  '<script id="trustedserver-js" src="/static/tsjs=tsjs-unified.min.js"></script>';

for (const placeholderId of [
  "fixture-header-placeholder",
  "real-gpt-placeholder",
]) {
  const placeholder = `<div id="${placeholderId}"></div>`;
  const html = configs.join("") + runtime + placeholder;

  test(`${placeholderId}: accepts the synchronous runtime with explicit origin and placeholder`, () => {
    assert.doesNotThrow(() => assertRuntimeOrder(html, origin, placeholderId));
    assert.doesNotThrow(() =>
      assertRuntimeOrder(
        html.replace('src="/static/', 'src="static/'),
        origin,
        placeholderId,
      ),
    );
    assert.throws(() => assertRuntimeOrder(html, "", placeholderId));
    assert.throws(() => assertRuntimeOrder(html, origin, "wrong-placeholder"));
    for (const api of [
      "publisherSlotRegistrationVersion",
      "tsRenderBridgeVersion",
    ]) {
      assert.doesNotThrow(() =>
        assertRuntimeOrder(
          html + `<script src="/${api}.js"></script>`,
          origin,
          placeholderId,
        ),
      );
    }
  });

  const invalid = [
    ["missing runtime", html.replace(runtime, "")],
    ["duplicate runtime", html + runtime],
    ["wrong runtime path", html.replace("tsjs-unified", "tsjs-gpt")],
    ...["async", "defer"].map((attribute) => [
      `${attribute} runtime`,
      html.replace(
        'id="trustedserver-js"',
        `${attribute} id="trustedserver-js"`,
      ),
    ]),
    ...configs.flatMap((config, index) => [
      [`missing config ${index}`, html.replace(config, "")],
      [`late config ${index}`, html.replace(config, "") + config],
    ]),
    ["late runtime", configs.join("") + placeholder + runtime],
    ["missing placeholder", html.replace(placeholder, "")],
    [
      "inline bootstrap",
      html + '<script id="trustedserver-bootstrap"></script>',
    ],
    ...["publisherSlotRegistrationVersion", "tsRenderBridgeVersion"].map(
      (api) => [
        `inline ${api} fallback`,
        html + `<script>window.tsjs={${api}:1};</script>`,
      ],
    ),
  ];
  for (const [name, invalidHtml] of invalid) {
    test(`${placeholderId}: rejects ${name}`, () => {
      assert.throws(() =>
        assertRuntimeOrder(invalidHtml, origin, placeholderId),
      );
    });
  }
}
