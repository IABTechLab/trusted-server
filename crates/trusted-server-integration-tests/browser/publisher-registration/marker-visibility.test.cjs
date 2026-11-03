const assert = require("node:assert/strict");
const { createServer } = require("node:http");
const { test } = require("node:test");
const { chromium } = require("@playwright/test");
const { marker, visiblyPainted } = require("./marker-visibility.cjs");

const text = "Visible fixture creative";
const style = `<style>html,body{margin:0;overflow:hidden}iframe{position:absolute;left:0;width:300px;height:250px;border:0;display:block}</style>`;

test("marker visibility clips through nested same-origin and cross-origin loopback frames", async (t) => {
  const origins = [];
  const servers = [];
  let browser;
  try {
    for (let index = 0; index < 2; index++) {
      const server = createServer((request, response) => {
        const url = new URL(request.url, origins[index]);
        const y = Number(url.searchParams.get("y") || 200);
        const cross = url.searchParams.get("cross") === "1";
        let body;
        if (url.pathname === "/top") {
          const outer = new URL("/outer", origins[cross ? 1 : 0]);
          outer.search = url.search;
          body = `<div id="placement"><iframe style="top:750px" src="${outer}"></iframe></div>`;
        } else if (url.pathname === "/outer") {
          const inner = new URL("/inner", origins[0]);
          inner.search = url.search;
          body = `<iframe style="top:0" src="${inner}"></iframe>`;
        } else if (url.pathname === "/inner") {
          body = `<div class="marker" style="position:absolute;top:${y}px;left:0;width:200px;height:20px;background:black;color:white">${text}</div>`;
        } else {
          response.writeHead(404).end();
          return;
        }
        response.writeHead(200, { "content-type": "text/html" });
        response.end(`<!doctype html>${style}${body}`);
      });
      servers.push(server);
      await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
      origins.push(`http://127.0.0.1:${server.address().port}`);
    }
    browser = await chromium.launch();
    const page = await browser.newPage({
      viewport: { width: 900, height: 800 },
    });
    const external = [];
    await page.route("**/*", (route) => {
      if (origins.includes(new URL(route.request().url()).origin))
        return route.continue();
      external.push(route.request().url());
      return route.abort();
    });
    for (const cross of [false, true]) {
      await t.test(
        cross ? "cross-origin chain" : "same-origin chain",
        async () => {
          for (const [y, expected] of [
            [200, false],
            [40, true],
            [20, true],
          ]) {
            await page.goto(`${origins[0]}/top?cross=${Number(cross)}&y=${y}`);
            const frame = page
              .frames()
              .find((entry) => new URL(entry.url()).pathname === "/inner");
            assert.ok(frame);
            assert.equal(
              new URL(frame.parentFrame().url()).origin,
              origins[cross ? 1 : 0],
            );
            const element = await frame.locator(".marker").elementHandle();
            // The old independent viewport checks accepted this exact clipped case.
            const locallyVisible = await element.evaluate((node) => {
              const rect = node.getBoundingClientRect();
              return rect.top < innerHeight && rect.bottom > 0;
            });
            assert.equal(locallyVisible, true);
            assert.equal(
              await visiblyPainted(await frame.parentFrame().frameElement()),
              true,
            );
            assert.equal(
              await visiblyPainted(element),
              expected,
              `marker y=${y}, cross=${cross}`,
            );
            assert.equal(await marker(page, text, "placement"), expected);
          }
          const outer = page.locator("#placement > iframe");
          for (const css of [
            "opacity:0",
            "visibility:hidden",
            "display:none",
            "content-visibility:hidden",
          ]) {
            await outer.evaluate(
              (node, css) => (node.style.cssText = `top:750px;${css}`),
              css,
            );
            assert.equal(await marker(page, text, "placement"), false, css);
          }
          await outer.evaluate((node) => (node.style.cssText = "top:750px"));
          assert.equal(await marker(page, text, "placement"), true);
          await page
            .locator("#placement")
            .evaluate(
              (node) =>
                (node.style.cssText =
                  "position:absolute;top:0;height:760px;width:300px;overflow:hidden"),
            );
          assert.equal(
            await marker(page, text, "placement"),
            false,
            "ancestor overflow clips marker but leaves part of frame visible",
          );
        },
      );
    }
    assert.deepEqual(external, []);
  } finally {
    await browser?.close();
    await Promise.all(
      servers.map((server) => new Promise((resolve) => server.close(resolve))),
    );
  }
});
