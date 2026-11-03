const test = require("node:test");
const assert = require("node:assert/strict");
const http = require("node:http");
const { fixtureProxy } = require("./proxy.cjs");
const listen = async (server) => {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  return `http://127.0.0.1:${server.address().port}`;
};
const close = (server) => new Promise((resolve) => server.close(resolve));
const request = (proxy, url, method = "GET", body = "") =>
  new Promise((resolve, reject) => {
    const outgoing = http.request(proxy, { path: url, method }, (response) => {
      let text = "";
      response.setEncoding("utf8");
      response.on("data", (chunk) => {
        text += chunk;
      });
      response.on("end", () =>
        resolve({
          status: response.statusCode,
          headers: response.headers,
          text,
        }),
      );
    });
    outgoing.on("error", reject);
    outgoing.end(body);
  });

test("proxy forwards only exact fixture origins and preserves HTTP cache headers and bodies", async () => {
  const observations = [];
  const upstreamRequests = [];
  const upstream = http.createServer((incoming, response) => {
    let body = "";
    incoming.on("data", (chunk) => {
      body += chunk;
    });
    incoming.on("end", () => {
      upstreamRequests.push({
        method: incoming.method,
        path: incoming.url,
        host: incoming.headers.host,
        body,
      });
      response.writeHead(200, {
        "Cache-Control": "public, max-age=3600",
        "Content-Type": "text/plain",
      });
      response.end("fixture-response");
    });
  });
  const origin = await listen(upstream);
  const creative = http.createServer((_request, response) =>
    response.end("creative"),
  );
  const creativeOrigin = await listen(creative);
  let forbiddenRequests = 0;
  const forbidden = http.createServer((_request, response) => {
    forbiddenRequests++;
    response.end("forbidden");
  });
  const forbiddenOrigin = await listen(forbidden);
  const proxy = fixtureProxy([origin, creativeOrigin], (entry) =>
    observations.push(entry),
  );
  const proxyOrigin = await listen(proxy);
  try {
    const response = await request(
      proxyOrigin,
      origin + "/runtime.js?q=1",
      "POST",
      "fixture-body",
    );
    assert.equal(response.status, 200);
    assert.equal(response.text, "fixture-response");
    assert.deepEqual(upstreamRequests, [
      {
        method: "POST",
        path: "/runtime.js?q=1",
        host: new URL(origin).host,
        body: "fixture-body",
      },
    ]);
    assert.equal(response.headers["content-type"], "text/plain");
    assert.equal(response.headers["cache-control"], "public, max-age=3600");
    assert.equal(
      (await request(proxyOrigin, creativeOrigin + "/creative")).text,
      "creative",
    );
    assert.equal(
      (await request(proxyOrigin, forbiddenOrigin + "/external")).status,
      403,
    );
    assert.equal(
      (await request(proxyOrigin, "http://publisher.example.com/external"))
        .status,
      403,
    );
    assert.equal((await request(proxyOrigin, "/relative")).status, 403);
    const suffixHost = new URL(origin);
    suffixHost.hostname = "127.0.0.1.example.com";
    assert.equal((await request(proxyOrigin, suffixHost.href)).status, 403);
    const wrongProtocol = new URL(origin);
    wrongProtocol.protocol = "https:";
    assert.equal((await request(proxyOrigin, wrongProtocol.href)).status, 403);
    assert.equal(forbiddenRequests, 0);
    assert.equal(
      observations.filter((entry) => entry.action === "forwarded").length,
      2,
    );
    assert.equal(
      observations.filter((entry) => entry.action === "rejected").length,
      5,
    );
  } finally {
    await Promise.all([
      close(proxy),
      close(upstream),
      close(creative),
      close(forbidden),
    ]);
  }
});

test("proxy rejects CONNECT without opening a tunnel", async () => {
  const observations = [];
  const proxy = fixtureProxy([], (entry) => observations.push(entry));
  const origin = await listen(proxy);
  try {
    const status = await new Promise((resolve, reject) => {
      const outgoing = http.request(origin, {
        method: "CONNECT",
        path: "publisher.example.com:443",
      });
      outgoing.on("connect", (response, socket) => {
        socket.destroy();
        resolve(response.statusCode);
      });
      outgoing.on("error", reject);
      outgoing.end();
    });
    assert.equal(status, 403);
    assert.deepEqual(
      observations.map(({ method, action }) => ({ method, action })),
      [{ method: "CONNECT", action: "rejected" }],
    );
  } finally {
    await close(proxy);
  }
});
