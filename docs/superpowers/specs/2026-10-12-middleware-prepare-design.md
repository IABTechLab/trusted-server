# Design Spec: A middleware may prepare, with the request's services, and may refuse

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#18, ready for review, answering IABTechLab/trusted-server#1281. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#18
**Last updated:** 2026-10-12

> **Why this spec exists.** `Middleware::create` is synchronous, is given no services, and is asked inside the synchronous streaming paths the Fastly adapter needs.

## 1. Problem

`Middleware::create` is synchronous, is given no services, and is asked inside the synchronous streaming paths the Fastly adapter needs. A middleware whose decision needs the key-value store or the network cannot decide there. An action can only change a body, so a middleware cannot answer a request with a status, which a well known file made from a service's answer needs when it has nothing to give, because an empty `robots.txt` tells crawlers to crawl everything.

## 2. Design

A middleware whose decision needs the request's services, or has to wait on the key-value store or the network, answers `Middleware::prepare` with a future. The request's handler runs it before the document is processed, as the middleware's module with the permissions it declares, and what it answers is kept under the middleware's name for `create` to take.

```rust
fn prepare<'a>(&'a self, call: ModuleCall<'a>, context: &'a MiddlewareContext<'a>) -> Option<Preparation<'a>> {
    Some(Box::pin(async move {
        let lines = self.lines_for(call, context.request_host).await?;
        Ok(Box::new(lines) as Box<dyn Any + Send>)
    }))
}

fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
    let lines: Vec<String> = context.prepared.take(MODULE).unwrap_or_default();
    MiddlewareAction { stream: Some(Box::new(AppendLines::new(lines))), ..MiddlewareAction::pass() }
}
```

A preparation may answer a `Refusal` instead, with a status, an optional `Retry-After` and a message, and the request is answered with that status, no document is processed, and nothing is stored or served from the store.

**Why.** `create` is synchronous, is given no services, and is asked inside the synchronous streaming paths that the Fastly adapter needs. A middleware that makes a well known file from what a store holds or a service answers, as robots.txt does, could not decide there, and could not fail closed when it had nothing to give, which for robots.txt matters because an empty file tells crawlers to crawl everything. The preparation runs where the handler is asynchronous on every adapter, and only its answer, a `Send` value, travels to the streaming paths, so `create`, the plan and every streaming path are as they were.

**The rules.** - A middleware that prepares nothing is asked `create` as before, and nothing of it changes.

- A preparation runs once per phase the document goes through, fetch and serve on a document fetched for the request, serve alone on a copy read from the store, in the order the entry names the middleware.
- The first refusal ends the preparations of a document, and the response it makes is marked private and unstorable.
- In the fetch phase a preparation must not depend on the reader, as `create` must not, because its answer may be stored with the document.

## 3. What is not in this change

- robots.txt as a middleware on this interface, which is the next pull request.
- A test of the preparation on a copy read from the store, which needs the template cache in the test services. The code path is the same function as the fetched document's, called with the serve phase alone.

## 4. Acceptance

The pull request's description records how it was checked, with every commit linted and tested on its own, in order, the full local check on its head and CI green on all 19 checks, and 5 deliberate faults made one at a time and each caught by a test.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
