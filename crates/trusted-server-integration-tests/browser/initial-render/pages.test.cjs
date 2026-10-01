const assert = require("node:assert/strict");
const { test } = require("node:test");
const { JSDOM } = require("jsdom");
const { creative, pucPage } = require("./pages.cjs");

const payload =
  '</script><script>window.injected=true</script><img src=x onerror="window.injected=true">&';

test("creative labels stay text and cannot create executable markup", () => {
  const dom = new JSDOM(creative(payload));
  assert.equal(
    dom.window.document.querySelector(".marker").textContent,
    payload,
  );
  assert.equal(dom.window.document.scripts.length, 1);
  assert.equal(dom.window.document.querySelector("img"), null);
  assert(!dom.window.document.scripts[0].textContent.includes(payload));
  dom.window.close();
});

test("PUC receives the ad ID as data from the URL, not inline code", () => {
  let received;
  const dom = new JSDOM(pucPage(), {
    url:
      "https://creative.example.com/fixture-puc?adId=" +
      encodeURIComponent(payload),
    runScripts: "dangerously",
    beforeParse(window) {
      window.ucTag = {
        renderAd(document, options) {
          received = options;
        },
      };
    },
  });
  assert.equal(received?.adId, payload);
  assert.equal(received?.pubUrl, "https://publisher.example.com");
  assert.equal(dom.window.injected, undefined);
  assert.equal(dom.window.document.querySelector("img"), null);
  dom.window.close();
});
