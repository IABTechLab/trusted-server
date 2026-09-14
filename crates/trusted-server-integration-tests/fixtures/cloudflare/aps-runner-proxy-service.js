const APS_RUNNER_URL =
  "https://client.aps.amazon-adsystem.com/prebid-creative.js";
const FORBIDDEN_HEADERS = [
  "authorization",
  "cookie",
  "forwarded",
  "referer",
  "x-forwarded-for",
  "x-publisher-secret",
];

export default {
  async fetch(request, environment) {
    const logicalUrl = request.headers.get("x-ts-aps-logical-url");
    const invalidRequest =
      request.method !== "GET" ||
      request.url !== APS_RUNNER_URL ||
      request.headers.get("accept-encoding") !== "identity" ||
      logicalUrl !== APS_RUNNER_URL ||
      FORBIDDEN_HEADERS.some((name) => request.headers.has(name));

    if (invalidRequest) {
      return new Response(null, { status: 500 });
    }

    const headers = new Headers();
    headers.set("accept-encoding", "identity");
    headers.set("x-ts-aps-logical-url", logicalUrl);
    return fetch(environment.APS_RUNNER_PROXY_TEST_ENDPOINT, {
      method: "GET",
      headers,
      redirect: "manual",
    });
  },
};
