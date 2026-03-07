import { test, expect } from "@playwright/test";
import { readState } from "../../helpers/state.js";

test.beforeEach(async ({}, testInfo) => {
  const state = readState();
  if (state.framework !== "nextjs") {
    testInfo.skip();
  }
});

test.describe("Next.js API route passthrough", () => {
  test("API route returns JSON without script injection", async ({
    request,
  }) => {
    const resp = await request.get("/api/hello");
    expect(resp.status()).toBe(200);

    const contentType = resp.headers()["content-type"] || "";
    expect(contentType).toContain("application/json");

    const body = await resp.json();
    expect(body.message).toBe("Hello from the API!");
    expect(body.status).toBe("success");

    // JSON must not contain HTML injection
    const text = await (await request.get("/api/hello")).text();
    expect(text).not.toContain("<script");
    expect(text).not.toContain("/static/tsjs=");
  });

  test("data API route returns structured JSON", async ({ request }) => {
    const resp = await request.get("/api/data");
    expect(resp.status()).toBe(200);

    const body = await resp.json();
    expect(body.users).toHaveLength(2);
    expect(body.users[0].name).toBe("Alice");
    expect(body.metadata.version).toBe("1.0.0");
  });
});
