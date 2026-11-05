import { spawn } from "node:child_process";
import { existsSync, writeFileSync, appendFileSync } from "node:fs";
import { setTimeout as delay } from "node:timers/promises";
import { assertBrowserBuildLease, runBrowserBuild } from "../browser-build-lease.mjs";

const [mode, root, ...args] = process.argv.slice(2);
if (mode === "run") {
  process.exitCode = await runBrowserBuild(args, { root });
} else if (mode === "guard") {
  await assertBrowserBuildLease(root);
  console.log("guard passed");
} else if (mode === "hold") {
  writeFileSync(args[0], String(process.pid));
  while (!existsSync(args[1])) await delay(20);
} else if (mode === "nested") {
  if (args[0] === "missing") delete process.env.TS_BROWSER_BUILD_LEASE;
  if (args[0] === "wrong") process.env.TS_BROWSER_BUILD_LEASE = "wrong";
  process.exitCode = await runBrowserBuild(
    [process.execPath, import.meta.filename, "guard", root], { root },
  );
} else if (mode === "orphan") {
  const child = spawn(process.execPath, [import.meta.filename, "hold", root, ...args], {
    stdio: "ignore",
  });
  child.unref();
} else if (mode === "zombie") {
  writeFileSync(`${root}/zombie-ready`, String(process.pid));
  while (!existsSync(`${root}/zombie-exit`)) await delay(5);
} else if (mode === "fork-race-command") {
  const child = spawn(process.execPath, [import.meta.filename, "fork-race-parent", root], { stdio: "ignore" });
  child.unref();
  while (!existsSync(`${root}/parent-ready`)) await delay(5);
} else if (mode === "fork-race-parent") {
  writeFileSync(`${root}/parent-ready`, String(process.pid));
  while (!existsSync(`${root}/fork`)) await delay(5);
  const child = spawn(process.execPath, [import.meta.filename, "hold", root, `${root}/leaf-ready`, `${root}/leaf-stop`], { stdio: "ignore" });
  child.unref();
} else if (mode === "log") {
  appendFileSync(args[0], `start ${process.pid}\n`);
  await delay(80);
  appendFileSync(args[0], `end ${process.pid}\n`);
} else if (mode === "cargo") {
  // A build-script process invokes the public module, then npm invokes it again.
  process.exitCode = await runBrowserBuild([process.execPath, args[0], "run", "nested"], { root });
} else if (mode === "exit") {
  process.exitCode = Number(args[0]);
} else {
  throw new Error(`Unknown fixture mode ${mode}`);
}
