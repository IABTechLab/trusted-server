#!/usr/bin/env node

import fs from "node:fs";

const documentPath = process.argv[2];
if (!documentPath || process.argv.length !== 3) {
  throw new Error(
    "usage: template-cache-verify-hard-cutover.mjs <served-document>",
  );
}

const html = fs.readFileSync(documentPath, "utf8");
const scripts = [
  ...html.matchAll(/<script([^>]*)>([\s\S]*?)<\/script>/giu),
].map(([, attributes, body]) => ({ attributes, body }));
const controllers = scripts.filter(({ body }) =>
  body.includes("const __TSJS_SERVER_BOOT_TRANSPORT_V1__="),
);
if (controllers.length !== 1) {
  throw new Error(
    `served document has ${controllers.length} hard-cutover controllers`,
  );
}

const assignment =
  /const __TSJS_SERVER_BOOT_TRANSPORT_V1__=("(?:\\.|[^"\\])*");/u.exec(
    controllers[0].body,
  );
if (!assignment?.[1])
  throw new Error("served controller has no sealed boot transport");

const transport = JSON.parse(JSON.parse(assignment[1]));
const boot = transport?.boot;
const projection = boot?.auctionProjection;
const manifest = boot?.manifest;
if (boot?.abi !== 1 || projection?.version !== 1 || manifest?.version !== 1) {
  throw new Error("served controller boot ABI is invalid");
}
if (manifest.firstDisplay !== null) {
  throw new Error(
    "template-cache fixture must use direct hard-cutover runtime boot",
  );
}
if (
  typeof manifest.runtimeSrc !== "string" ||
  !/^\/static\/tsjs=tsjs-unified\.min\.js\?v=[0-9a-f]{64}$/u.test(
    manifest.runtimeSrc,
  )
) {
  throw new Error("served controller runtime source is invalid");
}

const results = projection?.auction?.results;
const slots = projection?.slots;
const bids = projection?.bids;
if (!Array.isArray(results) || !Array.isArray(slots) || !Array.isArray(bids)) {
  throw new Error("served controller projection is invalid");
}
if (results.length !== 1 || slots.length !== 1 || bids.length !== 1) {
  throw new Error("served controller must carry one result, slot, and bid");
}
const result = results[0];
const slot = slots[0];
const bid = bids[0];
if (
  result?.outcome !== "winner" ||
  result.slot !== slot?.slot ||
  result.candidateId !== bid?.candidateId ||
  bid.slot !== slot.slot ||
  slot.slot !== "ts-slot-header" ||
  bid?.targeting?.hb_pb !== "4.25"
) {
  throw new Error("served controller detached the winning bid from its slot");
}

const selected = scripts.filter(({ attributes }) =>
  /(?:^|\s)id=(?:"trustedserver-js"|'trustedserver-js')(?:\s|$)/u.test(
    attributes,
  ),
);
if (selected.length !== 1) {
  throw new Error(
    `served document has ${selected.length} selected runtime scripts`,
  );
}
const selectedSource = /(?:^|\s)src=(?:"([^"]+)"|'([^']+)')(?:\s|$)/u.exec(
  selected[0].attributes,
);
if ((selectedSource?.[1] ?? selectedSource?.[2]) !== manifest.runtimeSrc) {
  throw new Error("served runtime script does not match the sealed manifest");
}

for (const legacy of ["var b=JSON.parse(", "var a=JSON.parse(", "s(b,a)"]) {
  if (html.includes(legacy))
    throw new Error(`legacy seam survived hard cutover: ${legacy}`);
}

process.stdout.write(
  `slots=${slots.length} bids=${bids.length} selected=${selected.length} mode=direct\n`,
);
