#!/usr/bin/env node
import { runBrowserBuild } from "./browser-build-lease.mjs";

const args = process.argv.slice(2);
try {
  if (args.shift() !== "--" || !args.length) {
    throw new Error("Usage: node scripts/browser-build-runner.mjs -- <command> [args...]");
  }
  process.exitCode = await runBrowserBuild(args);
} catch (error) {
  console.error(`Browser build lease: ${error.message}`);
  process.exitCode = 1;
}
