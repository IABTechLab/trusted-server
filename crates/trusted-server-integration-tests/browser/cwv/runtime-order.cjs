const assert = require("node:assert/strict");
const { scriptBlocks } = require("./html.cjs");

function assertRuntimeOrder(html, origin, placeholderId) {
  const scripts = scriptBlocks(html);
  const runtimes = scripts.filter((script) => script.id === "trustedserver-js");
  assert.equal(runtimes.length, 1);
  const runtime = runtimes[0];
  assert.equal(
    new URL(runtime.src, origin).pathname,
    "/static/tsjs=tsjs-unified.min.js",
  );
  assert.doesNotMatch(
    html.slice(runtime.startOffset, runtime.endOffset),
    /\s(?:async|defer)(?:\s|=|>)/,
  );
  for (const config of [
    "window.__tsjs_gpt_enabled=true",
    "window.__tsjs_prebid=",
  ]) {
    const script = scripts.find((entry) => entry.content.includes(config));
    assert.ok(script, `${config} must precede the synchronous runtime`);
    assert.ok(script.endOffset <= runtime.startOffset);
  }
  assert.ok(runtime.endOffset <= html.indexOf(`id="${placeholderId}"`));
  assert.equal(
    scripts.some((script) => script.id === "trustedserver-bootstrap"),
    false,
  );
  for (const api of [
    "publisherSlotRegistrationVersion",
    "tsRenderBridgeVersion",
  ]) {
    assert.equal(
      scripts.some((script) => !script.src && script.content.includes(api)),
      false,
      `no inline ${api} fallback`,
    );
  }
}

module.exports = { assertRuntimeOrder };
