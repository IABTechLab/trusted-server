#!/usr/bin/env node
import { spawn } from 'node:child_process';

const modes = ['core', 'ext', 'creative'];
const children = [];

function spawnWatcher(mode) {
  const child = spawn('npx', ['vite', 'build', '--mode', mode, '--watch'], {
    stdio: 'inherit',
    env: process.env,
  });
  child.on('exit', (code, signal) => {
    if (signal) {
      console.log(`[watch] mode ${mode} exited via signal ${signal}`);
    } else {
      console.log(`[watch] mode ${mode} exited with code ${code}`);
    }
  });
  children.push(child);
}

modes.forEach(spawnWatcher);

function shutdown(signal) {
  console.log(`\n[watch] received ${signal}; shutting down watchers...`);
  for (const child of children) {
    if (!child.killed) child.kill('SIGTERM');
  }
  process.exit(0);
}

process.on('SIGINT', () => shutdown('SIGINT'));
process.on('SIGTERM', () => shutdown('SIGTERM'));
