import { createHash } from "node:crypto";

export function stable(value) {
  if (Array.isArray(value)) return value.map(stable);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, stable(value[key])]),
    );
  }
  return value;
}
export const text = (value) => JSON.stringify(stable(value));
export const hash = (value) =>
  createHash("sha256").update(text(value)).digest("hex");
// Definition ownership helps diagnostics and widening classification, not signatures.
export function signatureHash({ definition, ...record }) {
  return hash(record);
}
const sorted = (values) =>
  [...new Set(values.map(text))].sort().map(JSON.parse);
const kindOf = (item) => Object.keys(item.inner)[0];
const publicKinds = new Set([
  "module",
  "struct",
  "enum",
  "union",
  "trait",
  "trait_alias",
  "function",
  "type_alias",
  "constant",
  "static",
  "macro",
  "proc_macro",
  "extern_type",
  "primitive",
]);

export function normalize(doc, profile) {
  if (doc.format_version !== 60) throw Error("requires rustdoc schema 60");
  const root = doc.index[doc.root];
  const lexical = new Map(
    Object.entries(doc.paths).map(([id, item]) => [
      String(id),
      item.path.join("::"),
    ]),
  );
  const records = new Map();
  const exportedIds = new Set();
  const referencedIds = new Set();
  const implicitPublicIds = new Set();
  let collectReferences = false;
  const item = (id) => doc.index[id];

  // Compiler-owned containment provides names for fields/members absent from paths.
  function names(id, path, ancestry = new Set()) {
    if (ancestry.has(String(id)) || !item(id)) return;
    ancestry = new Set([...ancestry, String(id)]);
    lexical.set(String(id), path);
    const node = item(id),
      kind = kindOf(node),
      body = node.inner[kind];
    if (kind === "module" || kind === "trait") {
      for (const child of body.items) {
        if (kind === "trait") implicitPublicIds.add(String(child));
        if (item(child)?.name)
          names(child, `${path}::${item(child).name}`, ancestry);
      }
    }
    if (kind === "enum")
      for (const child of body.variants) {
        implicitPublicIds.add(String(child));
        names(child, `${path}::${item(child).name}`, ancestry);
      }
    const fields =
      kind === "union"
        ? body.fields
        : (body.kind?.plain?.fields ??
          body.kind?.struct?.fields ??
          body.kind?.tuple);
    if (Array.isArray(fields))
      for (const child of fields) {
        if (kind === "variant" && child !== null)
          implicitPublicIds.add(String(child));
        if (child !== null)
          names(child, `${path}::${item(child).name}`, ancestry);
      }
    for (const impl of body.impls ?? []) {
      const { items, ...header } = item(impl).inner.impl;
      const implPath = `${path}::impl[${hash(semantic(header)).slice(0, 16)}]`;
      for (const child of items) {
        if (header.trait !== null) implicitPublicIds.add(String(child));
        if (item(child)?.name)
          lexical.set(String(child), `${implPath}::${item(child).name}`);
      }
    }
  }
  names(doc.root, root.name);
  // Public-only rustdoc strips private modules but retains reexported definitions.
  for (const [id, path] of [...lexical])
    if (item(id)?.crate_id === root.crate_id) names(id, path);
  collectReferences = true;
  function reference(id) {
    const path = lexical.get(String(id));
    if (!path)
      throw Error(
        `unresolved compiler reference ${id}; no fallback to numeric IDs`,
      );
    if (collectReferences && item(id)?.crate_id === root.crate_id)
      referencedIds.add(String(id));
    return path;
  }
  function semantic(value, key) {
    if (value === null || value === undefined) return value ?? null;
    if (Array.isArray(value)) {
      if (
        key === "inputs" &&
        value.every(
          (part) =>
            Array.isArray(part) &&
            part.length === 2 &&
            typeof part[0] === "string",
        )
      )
        return value.map(([, type]) => semantic(type));
      const result = value.map((part) => semantic(part));
      return ["bounds", "where_predicates", "provided_trait_methods"].includes(
        key,
      )
        ? sorted(result)
        : result;
    }
    if (typeof value !== "object") return value;
    const result = {};
    for (const [name, part] of Object.entries(value)) {
      if (name === "id") result.ref = part === null ? null : reference(part);
      else if (name === "path" && Object.hasOwn(value, "id")) continue;
      else if (name === "parent" && key === "restricted")
        result.parent = reference(part);
      else result[name] = semantic(part, name);
    }
    return stable(result);
  }
  function add(path, kind, definition, body, node, exposure = "export") {
    const record = stable({
      path,
      kind,
      definition,
      exposure,
      signature: semantic(body),
      attrs: sorted(
        (node.attrs ?? []).filter((attr) => {
          const source = typeof attr === "string" ? attr : (attr.other ?? "");
          // Rustdoc's cfg trace records source spans, not an exported contract.
          // Separate target/feature profiles already capture cfg visibility.
          return ![
            "doc(",
            "doc =",
            "#[doc(",
            "#[doc =",
            "#[attr = CfgTrace(",
          ].some((prefix) => source.startsWith(prefix));
        }),
      ),
      deprecation: semantic(node.deprecation),
      stability: semantic(node.stability),
      constStability: semantic(node.const_stability),
    });
    const key = `${kind} ${path}`;
    if (records.has(key) && text(records.get(key)) !== text(record))
      throw Error(`ambiguous export ${key}`);
    records.set(key, record);
  }
  function member(id, path, implicit, exposure) {
    const node = item(id);
    if (!node) throw Error(`missing member ${id}`);
    if (!implicit && node.visibility !== "public") return;
    const kind = kindOf(node);
    add(
      `${path}::${node.name}`,
      kind,
      lexical.get(String(id)),
      node.inner[kind],
      node,
      exposure,
    );
  }
  function impls(ids, path, exposure) {
    for (const id of ids ?? []) {
      const node = item(id),
        body = node?.inner.impl;
      if (!body) throw Error(`missing impl ${id}`);
      // Instantiated blanket copies belong to dependencies or to an original
      // local generic impl captured through its own trait. Not transitive semver.
      if (body.blanket_impl !== null) continue;
      const { items, ...header } = body;
      const normalized = semantic(header);
      const implPath = `${path}::impl[${hash(normalized).slice(0, 16)}]`;
      add(implPath, "impl", null, header, node, exposure);
      for (const child of items) {
        member(child, implPath, body.trait !== null, exposure);
      }
    }
  }
  function publish(id, path, ancestry = new Set(), exposure = "export") {
    const node = item(id);
    if (!node) throw Error(`missing definition for ${path}`);
    // Module reexport cycles, not an arbitrary depth limit.
    if (ancestry.has(String(id))) return;
    ancestry = new Set([...ancestry, String(id)]);
    const kind = kindOf(node),
      body = node.inner[kind];
    if (!publicKinds.has(kind))
      throw Error(`unsupported public item kind ${kind}`);
    exportedIds.add(String(id));
    const signature = { ...body };
    delete signature.impls;
    delete signature.implementations;
    if (kind === "module" || kind === "trait") delete signature.items;
    if (kind === "enum")
      signature.variants = body.variants.map((child) => item(child).name);
    function fields(ids, implicit) {
      return ids.map((child) => {
        const field = child === null ? null : item(child);
        if (!field || (!implicit && field.visibility !== "public"))
          return { private: true };
        member(child, path, true, exposure);
        return field.name;
      });
    }
    if (kind === "struct") {
      if (body.kind.plain)
        signature.kind = {
          plain: {
            ...body.kind.plain,
            fields: fields(body.kind.plain.fields, false),
          },
        };
      if (body.kind.tuple)
        signature.kind = { tuple: fields(body.kind.tuple, false) };
    }
    if (kind === "union") signature.fields = fields(body.fields, false);
    add(path, kind, lexical.get(String(id)), signature, node, exposure);
    if (kind === "module") children(id, path, ancestry, exposure);
    if (kind === "trait") {
      for (const child of body.items) member(child, path, true, exposure);
      impls(body.implementations, path, exposure);
    }
    if (kind === "enum")
      for (const child of body.variants) {
        const variant = item(child),
          value = structuredClone(variant.inner.variant);
        const variantPath = `${path}::${variant.name}`;
        function variantFields(ids) {
          return ids.map((field) => {
            member(field, variantPath, true, exposure);
            return item(field).name;
          });
        }
        if (value.kind.struct)
          value.kind.struct.fields = variantFields(value.kind.struct.fields);
        if (value.kind.tuple)
          value.kind.tuple = variantFields(value.kind.tuple);
        add(
          variantPath,
          "variant",
          lexical.get(String(child)),
          value,
          variant,
          exposure,
        );
      }
    impls(body.impls, path, exposure);
  }
  function children(moduleId, path, ancestry, exposure) {
    for (const child of item(moduleId).inner.module.items) {
      const node = item(child);
      if (node.visibility !== "public") continue;
      if (!node.inner.use) {
        publish(child, `${path}::${node.name}`, ancestry, exposure);
        continue;
      }
      const use = node.inner.use;
      if (use.id === null)
        throw Error(`unresolved reexport ${path}::${use.name}`);
      if (use.is_glob) {
        if (!item(use.id)?.inner.module)
          throw Error(
            `external/non-module glob requires dependency graph: ${use.source}`,
          );
        if (!ancestry.has(String(use.id)))
          children(
            use.id,
            path,
            new Set([...ancestry, String(use.id)]),
            exposure,
          );
      } else {
        const alias = `${path}::${use.name}`;
        add(
          alias,
          "reexport",
          null,
          { target: reference(use.id) },
          node,
          exposure,
        );
        if (item(use.id)?.crate_id === root.crate_id)
          publish(use.id, alias, ancestry, exposure);
      }
    }
  }
  publish(doc.root, root.name);
  for (const node of Object.values(doc.index))
    if (
      node.crate_id === root.crate_id &&
      kindOf(node) === "macro" &&
      node.attrs.some((attr) => attr === "macro_export")
    )
      publish(node.id, `${root.name}::${node.name}`);
  // Public signatures can expose a type with no nameable exported path.
  while (true) {
    const missing = [...referencedIds].filter(
      (id) => !exportedIds.has(id) && publicKinds.has(kindOf(item(id))),
    );
    if (!missing.length) break;
    for (const id of missing)
      publish(id, `reachable:${reference(id)}`, new Set(), "signature");
  }
  const inventory = Object.fromEntries(
    Object.values(doc.index)
      .filter(
        (node) =>
          node.crate_id === root.crate_id && lexical.has(String(node.id)),
      )
      .map((node) => [
        `${kindOf(node)} ${lexical.get(String(node.id))}`,
        {
          visibility: implicitPublicIds.has(String(node.id))
            ? "public"
            : semantic(node.visibility),
        },
      ]),
  );
  return stable({
    schema: 2,
    rustdocFormat: 60,
    privateInventory: doc.includes_private,
    profile,
    target: doc.target.triple,
    scope:
      "owned exports, explicit dependency bindings, signature reachability and auto traits; no instantiated blanket copies",
    enabledTargetFeatures: doc.target.target_features
      .filter((feature) => feature.globally_enabled)
      .map((feature) => feature.name)
      .sort(),
    records: Object.fromEntries(
      [...records].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)),
    ),
    inventory,
  });
}

export function compact(snapshot) {
  return {
    ...snapshot,
    inventory: snapshot.privateInventory
      ? Object.fromEntries(
          Object.entries(snapshot.inventory).filter(
            ([, value]) => value.visibility !== "public",
          ),
        )
      : {},
    records: Object.fromEntries(
      Object.entries(snapshot.records).map(([key, value]) => [
        key,
        signatureHash(value),
      ]),
    ),
  };
}
const fingerprint = (record) =>
  record === undefined
    ? null
    : typeof record === "string"
      ? record
      : signatureHash(record);

export function delta(before, after) {
  const metadata = ({ records, inventory, ...rest }) => rest;
  if (text(metadata(before)) !== text(metadata(after)))
    throw Error("snapshot provenance/profile/target mismatch");
  const result = [];
  for (const key of [
    ...new Set([...Object.keys(before.records), ...Object.keys(after.records)]),
  ].sort()) {
    const old = before.records[key],
      next = after.records[key];
    if (fingerprint(old) === fingerprint(next)) continue;
    const prior = next && before.inventory[`${next.kind} ${next.definition}`];
    const change = !old
      ? !before.privateInventory
        ? "add-or-widen"
        : prior && prior.visibility !== "public"
          ? "widen"
          : "add"
      : !next
        ? "remove"
        : "change";
    result.push({
      profile: before.profile,
      key,
      change,
      before: fingerprint(old),
      after: fingerprint(next),
    });
  }
  return result;
}

export function check(before, after, manifest) {
  const changes = delta(before, after);
  const seen = new Set();
  const digest = (value) =>
    value === null ||
    (typeof value === "string" && /^[a-f0-9]{64}$/.test(value));
  for (const row of manifest) {
    if (
      typeof row.key !== "string" ||
      !row.key ||
      row.key.includes("*") ||
      !digest(row.before) ||
      !digest(row.after) ||
      !["add", "add-or-widen", "widen", "remove", "change"].includes(
        row.change,
      ) ||
      ![undefined, "approved", "planned"].includes(row.state)
    )
      throw Error("invalid exact manifest row");
    const identity = `${row.profile} ${row.key}`;
    if (seen.has(identity)) throw Error("duplicate manifest identity");
    seen.add(identity);
    if (
      !["core-neutral", "integrations-facade", "transitional"].includes(
        row.class,
      ) ||
      typeof row.owner !== "string" ||
      !row.owner ||
      row.owner.includes("*") ||
      !Array.isArray(row.consumers) ||
      row.consumers.some(
        (consumer) =>
          typeof consumer !== "string" || !consumer || consumer.includes("*"),
      )
    )
      throw Error("incomplete exact owner/class/consumer metadata");
    if (
      row.class === "transitional" &&
      (!row.removal || !row.replacement || !row.step)
    )
      throw Error("transitional export needs removal/replacement/step");
  }
  const exact = (row, change) =>
    row.profile === change.profile &&
    row.key === change.key &&
    row.change === change.change &&
    row.before === change.before &&
    row.after === change.after;
  for (const change of changes) {
    if (
      !manifest.some((row) => row.state !== "planned" && exact(row, change))
    ) {
      throw Error(`undeclared exact API delta: ${change.change} ${change.key}`);
    }
  }
  for (const row of manifest)
    if (
      row.state !== "planned" &&
      !changes.some((change) => exact(row, change))
    )
      throw Error(`unused or stale approved row: ${row.key}`);
  return changes;
}
