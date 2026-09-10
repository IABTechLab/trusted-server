/**
 * Compile-time exhaustiveness guard for presentation switches.
 *
 * A switch that returns `string | undefined` silently accepts a new union
 * member by falling through to an implicit `undefined`, which renders as a
 * missing fact rather than a build failure. Routing the default arm here makes
 * the omission a type error while keeping the runtime result unchanged.
 */
export function unhandledCase(_value: never): undefined {
  return undefined;
}
