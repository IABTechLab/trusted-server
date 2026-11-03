const http = require("node:http");

// Proxy forwarding preserves Chromium's native HTTP cache. Only the two exact
// fixture origins may reach an upstream connection, regardless of browser bypass.
function fixtureProxy(origins, record) {
  const allowed = new Map(origins.map((origin) => [origin, new URL(origin)]));
  const server = http.createServer((request, response) => {
    let target;
    try {
      target = new URL(request.url);
    } catch {
      record({
        url: request.url,
        method: request.method,
        action: "rejected",
        reason: "Invalid proxy URL",
      });
      response.writeHead(403);
      response.end("External network forbidden in CWV fixture");
      return;
    }
    const destination = allowed.get(target.origin);
    if (!destination || target.protocol !== "http:") {
      record({
        url: target.href,
        method: request.method,
        action: "rejected",
        reason: "Origin not allowed",
      });
      response.writeHead(403);
      response.end("External network forbidden in CWV fixture");
      return;
    }
    const entry = {
      url: target.href,
      method: request.method,
      action: "forwarded",
    };
    record(entry);
    const headers = { ...request.headers, host: destination.host };
    delete headers["proxy-connection"];
    // Connection fields come only from the server-created origin map. The
    // request contributes a path, never an upstream hostname or port.
    const upstream = http.request(
      {
        hostname: destination.hostname,
        port: destination.port,
        path: target.pathname + target.search,
        method: request.method,
        headers,
      },
      (incoming) => {
        entry.status = incoming.statusCode;
        response.writeHead(incoming.statusCode, incoming.headers);
        incoming.pipe(response);
      },
    );
    upstream.on("error", (error) => {
      entry.error = error.message;
      if (!response.headersSent) response.writeHead(502);
      response.end("Fixture proxy upstream failed");
    });
    request.on("aborted", () => upstream.destroy());
    request.pipe(upstream);
  });
  server.on("connect", (request, socket) => {
    record({
      url: request.url,
      method: "CONNECT",
      action: "rejected",
      reason: "CONNECT forbidden",
    });
    socket.end("HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n");
  });
  return server;
}

module.exports = { fixtureProxy };
