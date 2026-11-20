// The mirror is request-local first-party data. Never put it in Prebid's User ID
// getter or global config: both survive cookie replacement and can echo to ingest.
const MIRROR_COOKIE_NAME = '__Host-ts-eids-mirror';
const MAX_MIRROR_BYTES = 3584;
type Fields = Record<string, unknown>;
type MirrorUid = Fields & { id: string; atype?: number };
type MirrorEid = Fields & { source: string; uids: MirrorUid[] };

function fields(value: unknown): Fields {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Fields)
    : {};
}

function readMirror(): MirrorEid[] {
  try {
    const cookie = document.cookie
      .split(';')
      .map((part) => part.trim())
      .find((part) => part.startsWith(`${MIRROR_COOKIE_NAME}=`))
      ?.slice(MIRROR_COOKIE_NAME.length + 1);
    if (!cookie || cookie.length > MAX_MIRROR_BYTES || !/^[\w-]+$/.test(cookie)) return [];
    const binary = atob(cookie.replace(/-/g, '+').replace(/_/g, '/'));
    const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
    const envelope = fields(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)));
    if (envelope.v !== 1 || !Array.isArray(envelope.eids)) return [];
    const now = Date.now() / 1000;
    return envelope.eids.flatMap((rawEid): MirrorEid[] => {
      const eid = fields(rawEid);
      if (typeof eid.source !== 'string' || !eid.source.trim() || !Array.isArray(eid.uids))
        return [];
      const uids = eid.uids.flatMap((rawUid): MirrorUid[] => {
        const uid = fields(rawUid);
        if (typeof uid.id !== 'string' || !uid.id.trim()) return [];
        if (
          uid.atype !== undefined &&
          (typeof uid.atype !== 'number' ||
            !Number.isInteger(uid.atype) ||
            uid.atype < 0 ||
            uid.atype > 2_147_483_647)
        )
          return [];
        if (
          'exp' in uid &&
          (typeof uid.exp !== 'number' ||
            !Number.isFinite(uid.exp) ||
            !Number.isInteger(uid.exp) ||
            uid.exp <= now)
        )
          return [];
        const copy = { ...uid };
        delete copy.exp;
        return [copy as MirrorUid];
      });
      return uids.length ? [{ ...eid, source: eid.source, uids }] : [];
    });
  } catch {
    return [];
  }
}

function eidLists(ortb2: unknown): unknown[][] {
  const user = fields(fields(ortb2).user);
  return [user.eids, fields(user.ext).eids].filter(Array.isArray);
}
function uidKey(source: unknown, uid: Fields): string {
  return JSON.stringify([source, uid.id, uid.atype]);
}

/**
 * Merge the current mirror into request-local OpenRTB data without mutating
 * publisher data. Configured global EIDs are consulted only for deduplication;
 * Prebid itself merges those arrays into the request on its normal path.
 */
export function mergeIdentityMirror(ortb2: unknown, getGlobalOrtb2: () => unknown): unknown {
  const mirror = readMirror();
  if (!mirror.length) return ortb2;
  const seen = new Set<string>();
  let globalOrtb2: unknown;
  try {
    globalOrtb2 = getGlobalOrtb2();
  } catch {
    return ortb2;
  }
  for (const list of [...eidLists(globalOrtb2), ...eidLists(ortb2)]) {
    for (const rawEid of list) {
      const eid = fields(rawEid);
      if (!Array.isArray(eid.uids)) continue;
      for (const uid of eid.uids) seen.add(uidKey(eid.source, fields(uid)));
    }
  }
  const request = fields(ortb2);
  const user = fields(request.user);
  const ext = fields(user.ext);
  const eids = Array.isArray(ext.eids) ? [...ext.eids] : [];
  let changed = false;
  for (const eid of mirror) {
    const uids = eid.uids.filter((uid) => {
      const key = uidKey(eid.source, uid);
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
    if (!uids.length) continue;
    // Keep distinct EID records so their inserter, matcher and ext metadata
    // remains associated with the UIDs it describes.
    eids.push({ ...eid, uids });
    changed = true;
  }
  return changed ? { ...request, user: { ...user, ext: { ...ext, eids } } } : ortb2;
}
