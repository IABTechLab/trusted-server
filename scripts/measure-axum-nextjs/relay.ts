import net from 'node:net';
import fs from 'node:fs';
import type { Socket } from 'node:net';

let next = 0;
const sockets = new Set<Socket>();
const out = fs.createWriteStream(new URL('./connections.jsonl', import.meta.url));
const log = (event) => {
  if (!out.writableEnded) {
    out.write(JSON.stringify({ ...event, at: performance.now() }) + '\n');
  }
};
const server = net.createServer((socket) => {
  const id = ++next;
  log({ event: 'open', id });
  const upstream = net.connect(18980, '127.0.0.1');
  sockets.add(socket);
  sockets.add(upstream);
  socket.on('data', (data) => log({ event: 'request-bytes', id, data: data.toString('base64') }));
  socket.on('error', (error) => log({ event: 'error', id, error: error.message }));
  upstream.on('error', (error) => {
    log({ event: 'upstream-error', id, error: error.message });
    socket.destroy();
  });
  socket.pipe(upstream);
  upstream.pipe(socket);
  socket.on('close', () => {
    log({ event: 'close', id });
    sockets.delete(socket);
    upstream.destroy();
  });
  upstream.on('close', () => {
    sockets.delete(upstream);
    socket.destroy();
  });
});
server.listen(18981, '127.0.0.1', () => log({ event: 'ready' }));

let stopping = false;
function stop() {
  if (stopping) return;
  stopping = true;
  server.close(() => out.end());
  for (const socket of sockets) socket.destroy();
}
process.on('SIGTERM', stop);
process.on('SIGINT', stop);
