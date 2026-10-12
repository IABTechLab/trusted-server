# Design Spec: Page changes as middleware

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#5, ready for review, answering IABTechLab/trusted-server#1269. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#5
**Last updated:** 2026-10-12

> **Why this spec exists.** A module changes a page through four hook traits, `IntegrationAttributeRewriter`, `IntegrationScriptRewriter`, `IntegrationHtmlStreamProcessorFactory` and `IntegrationHeadInjector`.

## 1. Problem

A module changes a page through four hook traits, `IntegrationAttributeRewriter`, `IntegrationScriptRewriter`, `IntegrationHtmlStreamProcessorFactory` and `IntegrationHeadInjector`. Every hook of a selected module runs on every page, in the one order the modules were registered in. A publisher cannot run a change on part of a site, cannot choose the order, and cannot place two changes of one module apart. Whether a hook's output may be stored in a template every reader shares rests on what the hook says about itself.

## 2. Design

**The problem.** A module changed a page through four hook traits, `IntegrationAttributeRewriter`, `IntegrationScriptRewriter`, `IntegrationHtmlStreamProcessorFactory` and `IntegrationHeadInjector`. Every hook of a selected module ran on every page, in the one order the modules were registered in. A publisher could not run a change on part of a site, could not choose the order, and could not place two changes of one module apart. Whether a hook's output could be stored in a shared template rested on what the hook declared about itself.

```toml
[[fetch]]
media_type = "text/html"
path = "/news/"
middleware = ["tag.example.cleanup", "tag.example"]

[[fetch]]
media_type = "text/html"
middleware = ["tag.example"]

[[serve]]
media_type = "text/html"
middleware = ["tag.example.reader"]
```

Each phase has an ordered list of entries. An entry covers `text/html`, optionally only the requests under a path prefix, and names the middleware to run in order. A page takes the first entry that covers it. An entry places and orders, and switches nothing on. The section that selects a module still decides whether it runs.

| Phase       | Runs                                                                           | Told about the reader | Stored in a shared template |
| ----------- | ------------------------------------------------------------------------------ | --------------------- | --------------------------- |
| `[[fetch]]` | On the page as the origin sent it                                              | No                    | Yes                         |
| `[[serve]]` | On each reader's copy, whether the page came from the store or from the origin | Yes                   | No                          |

The contract is `trusted_server_core::middleware`. A middleware is asked once for each document and answers with any of five things.

| Part                        | What it does                                                                                               |
| --------------------------- | ---------------------------------------------------------------------------------------------------------- |
| Head markup                 | Written in `<head>` ahead of the script bundle                                                             |
| Markup for after the bundle | Written straight after the bundle's tag                                                                    |
| Element handlers            | For the elements a selector matches, keep the element, replace one attribute's value or remove the element |
| Text handlers               | For the text inside the elements a selector matches, keep a chunk, replace it or remove it                 |
| Stream processor            | Runs over the document the handlers left                                                                   |

A registration offers one with `.with_middleware(...)`. The guide has two fixtures a vendor can copy, and the documentation test compiles both as written.

**What converted.** Each conversion is one commit. The first commit of this pull request recorded the page each module produced through its hooks, as a file in the module's crate, and every conversion leaves its recording byte for byte as it was.

| Module                          | Through hooks it had                                                                  | Now supplies                                                               |
| ------------------------------- | ------------------------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| Prebid                          | An attribute rewriter and a head injector                                             | `auction.prebid`, fetch                                                    |
| JavaScript asset proxy, in core | An attribute rewriter                                                                 | `js_asset_proxy`, fetch                                                    |
| Testlight                       | An attribute rewriter                                                                 | `testing.testlight`, fetch                                                 |
| Next.js                         | Two script rewriters and a stream processor                                           | `framework.nextjs`, fetch                                                  |
| Permutive                       | An attribute rewriter                                                                 | `audience.permutive`, fetch                                                |
| lockr                           | An attribute rewriter                                                                 | `identity.lockr`, fetch                                                    |
| Didomi                          | A head injector                                                                       | `cmp.didomi`, fetch                                                        |
| Sourcepoint                     | An attribute rewriter and a head injector                                             | `cmp.sourcepoint`, fetch                                                   |
| Google Tag Manager              | An attribute rewriter and a script rewriter                                           | `tag.google-tag-manager`, fetch                                            |
| DataDome                        | An attribute rewriter, and a head injector that reads what its request filter decided | `bot-protection.datadome`, fetch, and `bot-protection.datadome.tag`, serve |
| Google Publisher Tags           | An attribute rewriter and a head injector                                             | `ad-tag.google`, fetch                                                     |
| GPT diagnostics                 | A head injector that reads what its request preparer decided                          | `ad-tag.google.diagnostics`, serve                                         |
| APS                             | A head injector that wrote no markup and set one attribute on the bundle's tag        | No middleware. It states the attribute with `with_bundle_tag_attribute`    |

Twenty implementations of the four traits became thirteen middleware, because a module's hooks that work on one page are one page change.

**What is removed.** - `IntegrationAttributeRewriter`, `IntegrationScriptRewriter`, `IntegrationHtmlStreamProcessorFactory` and `IntegrationHeadInjector`, with their four context types.

- The registration methods that took them, and the registry's four lists.
- `AttributeRewriteOutcome` and `ScriptTextAccumulator`. A text handler is made for one document and keeps its own text, so the per-document buffer that guarded against one document's text reaching the next has nothing left to guard.
- The function that handed a shared template no request state. What a request left reaches a document through the `[[serve]]` entry alone, which does not run on the pass that builds a template every reader shares, and a fetch middleware is handed none of it. This is the rule section 3.7 said would become structural.

The document processor no longer asks the registry anything about a page. It moves the origin's address in the five attributes it always has, writes the bundle, and carries out the plan of the middleware an entry names.

**This is a breaking change.** A deployment that selects a module and writes no entry gets no page change from it, and startup logs a warning that names each middleware no entry names.

- `trusted-server.example.toml` documents one `[[fetch]]` and one `[[serve]]` entry naming every middleware the stock modules supply, in the order the modules' hooks ran in. A deployment keeps the names of the modules it selects.
- `ts audit generate` writes the entries for the modules it selects, in that order.
- `crates/trusted-server-modules` lists the stock middleware (`middleware()`, `middleware_for(...)`). Its tests hold the list to what the modules register, and hold the example configuration and the configuration guide's table to the list.

One order is worth knowing. The script proxy knows a script by the address it is handed. It ran ahead of every module a section selects, and it now runs where the entry puts it, so it is named ahead of a middleware that moves the address of a script it is to proxy or block.

**Where the serve entry runs.** A page leaves from four places, and the serve entry runs at each.

1. A page that is not stored runs it on the same pass as the fetch entry, after it, so a streamed page is still streamed.
2. A page transformed as a shared template runs it once the template is stored and this reader's copy is assembled.
3. A page from the store runs it after assembly in the buffered finalizer.
4. A page from the store runs it piece by piece in the streaming finalizer, so the head still goes out before the auction is awaited.

A page from the store keeps no record of its origin's host. A serve middleware is told the host from the settings, so it is told the same on a hit as on a miss.

| When                                             | What is refused                                                                                                                                                                                                                                                                                   |
| ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| When the settings load, and at deploy validation | An entry for a media type other than `text/html`, a path that is not a request path prefix, an entry that names nothing or names one middleware twice, a key an entry does not read, an entry an earlier one already covers, and two entries for the same pages, which are told to share one list |
| When the service starts                          | An entry naming a middleware that no running module supplies, with the names that could be written. An entry naming a middleware in the phase it does not run in. Two registrations that supply one name                                                                                          |
| When a page is served                            | A handler whose selector does not parse. The response fails with a `500` that names the middleware, before a byte of the page is sent, where the parser would otherwise panic                                                                                                                     |

A name is checked when the service starts and not at deploy validation, which is where a section's module names are checked and for the same reason. A crate the `ts` tool never links may supply the name.

**Three faults found on the way.** Recording the pages showed two faults in how a rewritten inline script is written back, both on `main` as well, and CI on Windows showed a third. Each has tests that fail without its fix.

1. A script a rewriter replaced was written back as text, which escapes `&`, `<` and `>`. A browser decodes no entity inside a `<script>`, so the standard Google Tag Manager snippet's `'&l='+l` reached the page as `'&amp;l='+l`, and a loader holding `&&` or `<` became a syntax error. It reached a rewritten `__NEXT_DATA__` holding those characters too.
2. The Google Tag Manager module holds a script's text back while a marker might still arrive. A part that arrived after the held text had stopped looking like a marker went out ahead of it, so a script such as `footer.innerHTML = '<b>Search with google</b> <i>or</i> <u>another</u> engine';` reached the page out of order.
3. The GPT bootstrap script is embedded with `include_str!` and written into the page. A checkout that rewrites line endings changed the bytes of the page, which a Windows runner caught. The file is pinned to one set of line endings.

The first two commits stand alone and can be taken on their own.

One gap in the tests was found by breaking the code on purpose. Core moves the origin's address in `srcset` and `imagesrcset`, and no test said so. A test now does, added before the hooks were removed and passing on both sides of the removal.

**Where this differs from the text of section 3.7.** The design text was written before this was built. Five things came out differently, and the text needs to follow.

1. **Names.** A middleware is named by its module's name, as `module_name!()` gives it, with a part of its own after it where a module supplies several, such as `bot-protection.datadome.tag`. The text says `<integration>.<local-middleware>`. An entry writes the name in full.
2. **Settings.** A middleware reads its settings from its module's own `[<type>.<name>]` table, which its build function is already handed. The text says `[middleware.<name>]`. Nothing new is needed, and a crate that supplies only a middleware is selected like a crate of any other type.
3. **A fifth part.** `IntegrationHeadInjector` could write markup straight after the script bundle, which the text's table of four parts leaves out. The contract carries it.
4. **Twenty implementations, two of them serve.** The text counts nineteen and one serve conversion. Moving the modules out of core added the GPT diagnostics head injector, which reads what its request preparer left and so is a serve middleware, as DataDome's client tag is.
5. **Where serve markup goes.** A serve middleware's head markup goes straight before the script bundle, after the fetch middleware's, where the text says the start of `<head>`. A module that converts keeps its place among the head markup that way, and DataDome's recorded page does not change.

| **Commits.**                                                                                                            | Commits                                                                                                                                             | What they do |
| ----------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- | ------------ |
| 9ee13c535                                                                                                               | Records the page each module's hooks produce, so that a conversion can be shown to leave the page as it was                                         |
| 68cb6c372, 950003760                                                                                                    | The two faults in how a rewritten script is written back                                                                                            |
| 0b21fe357, 4600b1cdd                                                                                                    | The contract, the entries, the registry's checks, and the fetch and serve phases at the four places a page leaves from                              |
| 9b4cb4cfb, e0bf6f27c, e17e05847                                                                                         | The Fastly probe's serve test, the refusal that tells two entries for the same pages to share a list, and helpers for a module's own tests          |
| 47ab129b7                                                                                                               | The GPT bootstrap script's line endings                                                                                                             |
| 04c7bf304, 1e36ef207, b37c64b70, 33d679081, b97c5b4a0, c64c9c4d0, a6c4be137, 4d6e7b234, cb7218634, 847a02d5c, d81b56b7c | One module each, in the order Testlight, Permutive, lockr, Didomi, Sourcepoint, Google Tag Manager, DataDome, Prebid, GPT, GPT diagnostics, Next.js |
| 60732a75b, 99066ff1b                                                                                                    | A registration states the attributes it puts on the bundle's tag, and APS does so in place of its head injector                                     |
| d4d5fe142                                                                                                               | Core's JavaScript asset proxy                                                                                                                       |
| 0b6c1e319, 072e3620e                                                                                                    | Core's own test stand-ins, the one that acts on a request as a serve middleware                                                                     |
| a00b0052a                                                                                                               | The test of `srcset` and `imagesrcset`                                                                                                              |
| 7e039ae73                                                                                                               | The four hook traits are removed                                                                                                                    |
| 3352f8de6, 21623bb5b                                                                                                    | The stock list of middleware, `ts audit`, the example configuration and the guide's table                                                           |
| c4c40764f                                                                                                               | Tests and comments named for a hook are named for the middleware                                                                                    |

## 3. What is not in this change

Nothing beyond the design above. A deployment whose settings do not select the change behaves as before.

## 4. Acceptance

The pull request's description records how it was checked, with CI green on all 19 checks.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
