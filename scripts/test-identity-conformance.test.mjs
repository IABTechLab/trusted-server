import assert from "node:assert/strict";
import test from "node:test";
import { runSuite } from "./test-identity-conformance.mjs";

const testName = "identity_conformance::identity_conformance_fastly";

test("an unavailable adapter fails before invoking cargo", () => {
  let invoked = false;
  assert.throws(
    () =>
      runSuite("axum", () => {
        invoked = true;
        return { stdout: "" };
      }),
    /Axum.*#1574/,
  );
  assert.equal(invoked, false);
});

test("an unknown adapter fails explicitly", () => {
  assert.throws(
    () => runSuite("example", () => ({ stdout: "" })),
    /Unknown adapter/,
  );
});

test("a successful cargo command listing no conformance test is rejected", () => {
  assert.throws(
    () => runSuite("fastly", () => ({ stdout: "0 tests, 0 benchmarks\n" })),
    /Missing required/,
  );
});

test("a zero-test execution is rejected after a successful listing", () => {
  let calls = 0;
  assert.throws(
    () =>
      runSuite("fastly", () => ({
        stdout:
          ++calls === 1
            ? `${testName}: test\n`
            : "test result: ok. 0 passed; 0 failed; 0 ignored;\n",
      })),
    /did not execute/,
  );
});

test("the exact listed conformance test must pass", () => {
  const commands = [];
  const output = runSuite("fastly", (args) => {
    commands.push(args);
    return {
      stdout:
        commands.length === 1
          ? `${testName}: test\n`
          : "test result: ok. 1 passed; 0 failed; 0 ignored;\n",
    };
  });
  assert.match(output, /1 passed/);
  assert.ok(commands[0].includes("--list"));
  assert.ok(commands[1].includes("--exact"));
  assert.ok(commands.every((command) => command.includes(testName)));
});
