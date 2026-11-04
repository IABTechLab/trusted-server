import assert from "node:assert/strict";
import test from "node:test";
import { normalize, delta, check, text, compact } from "./normalize.mjs";
import { compile, source, withAddedFunction } from "./test-fixture.mjs";

const baselineDoc = compile(source);
const baseline = normalize(baselineDoc, "native/default");
function denied(candidate) {
  const snapshot = normalize(compile(candidate), "native/default");
  assert.throws(
    () => check(baseline, snapshot, []),
    /undeclared exact API delta/,
  );
  return delta(baseline, snapshot);
}

test("source order, line positions, ordinary bodies and documentation do not change snapshot", () => {
  const block =
    "pub mod visible {\n    pub struct Other;\n    pub type Name = u64;\n}\n";
  const edited =
    source
      .replace(block, "")
      .replace(
        "Service, Thing as PublicThing",
        "Thing as PublicThing, Service",
      ) +
    "\n" +
    block;
  const doc = compile(
    edited
      .replace(
        "//! Public API gate fixture.",
        "//! Different documentation, extra lines.\n\n\n",
      )
      .replace(
        "pub struct Tuple",
        "/// Newly documented type.\npub struct Tuple",
      )
      .replace(
        "pub fn hidden_export() {}",
        "pub fn hidden_export() { let _ = 9; }",
      ),
  );
  assert.equal(text(baseline), text(normalize(doc, "native/default")));
  const moved = structuredClone(baseline);
  for (const record of Object.values(moved.records))
    record.definition = "different::owning_module";
  assert.deepEqual(check(compact(baseline), moved, []), []);
});

test("renumbering every rustdoc item ID does not change records", () => {
  const ids = new Set([
    ...Object.keys(baselineDoc.index),
    ...Object.keys(baselineDoc.paths),
  ]);
  const remap = (id) => (ids.has(String(id)) ? 10000000 - Number(id) : id);
  function visit(value, key) {
    if (Array.isArray(value)) {
      if (
        ["items", "impls", "implementations", "fields", "variants"].includes(
          key,
        ) ||
        (key === "tuple" &&
          value.every((part) => typeof part === "number" || part === null))
      )
        return value.map((id) => (id === null ? null : remap(id)));
      return value.map((part) => visit(part));
    }
    if (!value || typeof value !== "object")
      return ["id", "parent", "root"].includes(key) ? remap(value) : value;
    return Object.fromEntries(
      Object.entries(value).map(([name, part]) => [
        key === "index" || key === "paths" ? String(remap(name)) : name,
        visit(part, name),
      ]),
    );
  }
  const changed = visit(baselineDoc);
  assert.notEqual(changed.root, baselineDoc.root);
  assert.equal(text(normalize(changed, "native/default")), text(baseline));
});

test("undeclared added public canary fails", () => {
  assert(
    denied(source + "\npub fn added_canary() {}\n").some(
      (row) => row.change === "add" && row.key.endsWith("::added_canary"),
    ),
  );
});
test("undeclared removed public canary fails", () => {
  assert(
    denied(source.replace("pub fn hidden_export() {}", "")).some(
      (row) => row.change === "remove" && row.key.endsWith("::hidden_export"),
    ),
  );
});
test("compiler private inventory distinguishes visibility widening", () => {
  assert(
    denied(
      source.replace("pub(crate) fn widening_canary", "pub fn widening_canary"),
    ).some(
      (row) => row.change === "widen" && row.key.endsWith("::widening_canary"),
    ),
  );
});
test("public-only JSON still blocks widening as add-or-widen", () => {
  const before = normalize(
    compile(source, { privateItems: false }),
    "native/default",
  );
  const after = normalize(
    compile(
      source.replace("pub(crate) fn widening_canary", "pub fn widening_canary"),
      { privateItems: false },
    ),
    "native/default",
  );
  assert.throws(() => check(before, after, []), /undeclared exact API delta/);
  assert(
    delta(before, after).some(
      (row) =>
        row.change === "add-or-widen" && row.key.endsWith("::widening_canary"),
    ),
  );
});
test("function signature changes fail", () =>
  denied(source.replace("pub fn make()", "pub fn make(_: u16)")));
test("public enum fields and variant additions fail", () => {
  assert(
    denied(source.replace("Data { value: u8 }", "Data { value: u16 }")).some(
      (row) => row.key.includes("Event::Data::value"),
    ),
  );
  assert(
    denied(source.replace("    Unit,", "    Unit,\n    NewVariant,")).some(
      (row) => row.key.endsWith("Event::NewVariant"),
    ),
  );
});
test("inherent methods and associated constants fail", () => {
  assert(
    denied(
      source.replace("pub fn get(&self)", "pub fn get(&self, _: u16)"),
    ).some((row) => row.key.endsWith("::get")),
  );
  assert(
    denied(source.replace("LIMIT: usize = 4", "LIMIT: usize = 5")).some((row) =>
      row.key.endsWith("::LIMIT"),
    ),
  );
});
test("trait members and implementations fail", () => {
  assert(
    denied(source.replace("type Output;", "type Output: Clone;")).some((row) =>
      row.key.endsWith("Service::Output"),
    ),
  );
  assert(
    denied(
      source.replace("fn defaulted(&self)", "fn defaulted(&self, _: u16)"),
    ).some((row) => row.key.endsWith("Service::defaulted")),
  );
  const block =
    "    impl<T: Clone> Service for Thing<T> {\n        type Output = T;\n        fn run(&self, _: u8) -> T {\n            self.value.clone()\n        }\n    }\n";
  assert(
    source.includes(block),
    "should remove the actual formatted trait implementation",
  );
  assert(
    denied(source.replace(block, "")).some((row) =>
      row.key.startsWith("impl "),
    ),
  );
});
test("local alias and glob routes are real exported records", () => {
  assert(baseline.records["struct trusted_server_core::PublicThing"]);
  assert(
    baseline.records["struct_field trusted_server_core::PublicThing::value"],
  );
  assert(baseline.records["trait trusted_server_core::Service"]);
  assert(baseline.records["struct trusted_server_core::Other"]);
  assert(
    denied(source.replace("pub use visible::*;", "")).some(
      (row) =>
        row.change === "remove" &&
        row.key === "struct trusted_server_core::Other",
    ),
  );
  assert(
    denied(source.replaceAll("PublicThing", "RenamedThing")).some(
      (row) =>
        row.change === "remove" &&
        row.key === "struct trusted_server_core::PublicThing",
    ),
  );
});
test("explicit external reexports preserve binding; unresolved external globs fail closed", () => {
  const explicit = normalize(
    compile(source + "\npub use std::collections::BTreeMap as ExternalMap;\n"),
    "native/default",
  );
  assert(
    explicit.records[
      "reexport trusted_server_core::ExternalMap"
    ].signature.target.includes("BTreeMap"),
  );
  assert.throws(
    () =>
      normalize(
        compile(source + "\npub use std::collections::*;\n"),
        "native/default",
      ),
    /external\/non-module glob/,
  );
});
test("signature-reachable unnameable public types retain public members", () => {
  const extra =
    source.replace(
      "mod private_owner {",
      "mod private_owner {\n    pub struct HiddenOutput { pub answer: u8 }",
    ) +
    "\npub fn hidden_result() -> private_owner::HiddenOutput { unimplemented!() }\n";
  const before = normalize(compile(extra), "native/default");
  const key =
    "struct_field reachable:trusted_server_core::private_owner::HiddenOutput::answer";
  assert(before.records[key]);
  const after = normalize(
    compile(extra.replace("pub answer: u8", "pub answer: u16")),
    "native/default",
  );
  assert.throws(() => check(before, after, []), /undeclared exact API delta/);
});
test("target and feature profiles are separate, never unioned", () => {
  const wasm = normalize(
    compile(source, { target: "wasm32-wasip1" }),
    "wasm/default",
  );
  assert(wasm.records["function trusted_server_core::target_only"]);
  assert(!baseline.records["function trusted_server_core::target_only"]);
  assert.throws(() => delta(baseline, wasm), /profile\/target mismatch/);
  const withFeature =
    source + '\n#[cfg(feature = "test-utils")]\npub fn feature_only() {}\n';
  const enabled = normalize(
    compile(withFeature, { cfg: 'feature="test-utils"' }),
    "native/test-utils",
  );
  const disabled = normalize(compile(withFeature), "native/default");
  assert(enabled.records["function trusted_server_core::feature_only"]);
  assert(!disabled.records["function trusted_server_core::feature_only"]);
});
test("exact approved manifest passes, drift and incomplete/stale approvals fail", () => {
  const after = normalize(
    compile(source + "\npub fn added_canary() {}\n"),
    "native/default",
  );
  const changes = delta(baseline, after);
  const manifest = changes.map((row) => ({
    ...row,
    owner: "trusted_server_core",
    class: "core-neutral",
    consumers: ["fixture-consumer"],
    removal: null,
  }));
  assert.deepEqual(check(baseline, after, manifest), changes);
  assert.deepEqual(check(compact(baseline), after, manifest), changes);
  assert.throws(
    () =>
      check(
        baseline,
        after,
        manifest.map((row) => ({ ...row, after: "f".repeat(64) })),
      ),
    /undeclared exact API delta/,
  );
  assert.throws(
    () => check(baseline, baseline, manifest),
    /stale approved row/,
  );
  assert.throws(
    () =>
      check(
        baseline,
        after,
        manifest.map((row) => ({ ...row, class: "transitional" })),
      ),
    /removal\/replacement\/step/,
  );
  assert.throws(
    () =>
      check(
        baseline,
        after,
        manifest.map((row) => ({ ...row, state: "planned" })),
      ),
    /undeclared exact API delta/,
  );
  assert.throws(
    () =>
      check(
        baseline,
        after,
        manifest.map((row) => ({
          ...row,
          key: "function trusted_server_core::*",
        })),
      ),
    /invalid exact manifest row/,
  );
  assert.throws(
    () =>
      check(
        baseline,
        after,
        manifest.map((row) => ({ ...row, owner: "trusted_server_core::*" })),
      ),
    /exact owner/,
  );
  assert.throws(
    () =>
      check(
        baseline,
        after,
        manifest.map((row) => ({ ...row, consumers: ["consumer::*"] })),
      ),
    /exact owner/,
  );
});
test("unknown schema and unresolved compiler references fail instead of dropping IDs", () => {
  assert.throws(
    () => normalize({ ...baselineDoc, format_version: 61 }, "native/default"),
    /schema 60/,
  );
  const bad = structuredClone(baselineDoc);
  const fn = Object.values(bad.index).find((item) => item.name === "make");
  fn.inner.function.sig.output.resolved_path.id = 999999999;
  assert.throws(
    () => normalize(bad, "native/default"),
    /unresolved compiler reference/,
  );
});

test("compiler-model export insertion and module removal fail", () => {
  const changed = withAddedFunction(baselineDoc, "api_canary");
  assert.throws(
    () => check(baseline, normalize(changed, "native/default"), []),
    /api_canary/,
  );
  const removed = structuredClone(baselineDoc);
  const rootModule = removed.index[removed.root].inner.module;
  rootModule.items = rootModule.items.filter(
    (id) => removed.index[id].name !== "visible",
  );
  assert.throws(
    () => check(baseline, normalize(removed, "native/default"), []),
    /undeclared exact API delta/,
  );
});
