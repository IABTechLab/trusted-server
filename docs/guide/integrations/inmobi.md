# InMobi Choice Integration

**Category**: Consent Management Platform

**Status**: Development

**Type**: Consent prompt tag, with the IAB stubs ahead of it

## Overview

InMobi Choice is a TCF consent prompt loaded from the publisher's own InMobi
account. The module writes the prompt's tag into the head of each page a
`[[fetch]]` or `[[serve]]` entry for `text/html` names it for, in three parts
and in this order:

1. The IAB TCF v2 stub, which creates the `__tcfapiLocator` frame, queues
   every `__tcfapi` call made before the prompt arrives, answers `ping` as a
   stub, and relays calls posted from frames.
2. InMobi's GPP stub, which answers `ping` and the other generic `__gpp`
   commands at once with the configured CMP id, queues every other call for
   the prompt, creates the `__gppLocator` frame, and relays calls posted
   from frames.
3. The account's Choice loader, deferred, so a slow consent vendor never
   holds up the page a reader came for and the loader never runs ahead of a
   stub the publisher's own head carries.

Each stub defines nothing where a page already has its interface. Nothing
else of the page is changed, and the module serves no browser module of its
own, because the two stubs and the Choice script are the whole client side.

The prompt writes the reader's answer to the `euconsent-v2` cookie, which
core reads on the next request. The TCF permission signal module reads the
record core decodes from it, so the answer reaches the permission model the
way every signal does.

## Configuration

```toml
[cmp]
module = "inmobi"

[cmp.inmobi]
script_url = "https://cmp.inmobi.com/choice/<account>/<site>/choice.js?tag_version=V3"
# cmp_id = 10

[[fetch]]
media_type = "text/html"
middleware = ["cmp.inmobi", "tag.google-tag-manager"]
```

| Field        | Type   | Default  | Contract                                                                                                        |
| ------------ | ------ | -------- | --------------------------------------------------------------------------------------------------------------- |
| `script_url` | URL    | Required | The account's Choice loader, `https` on `cmp.inmobi.com` or `cdn.inmobi.com`, with no user name or password     |
| `cmp_id`     | Number | `10`     | The IAB registered CMP identifier, from 1 to 4095, that the GPP stub answers `ping` with while the prompt loads |

The table refuses a field it does not read.

## Placing the prompt

The prompt changes a page only where a `[[fetch]]` or `[[serve]]` entry
names `cmp.inmobi`, in the phase the publisher chooses. The tag is the same
for every reader, so the fetch phase stores it in the page every reader is
served from, which is what `ts audit generate` writes, and the serve phase
writes it on each reader's copy instead. Core writes each
middleware's head markup in the order the entry names them, and nothing
advertising related may run before the reader answers the prompt, so name
`cmp.inmobi` before any middleware that writes a vendor's tag, as the example
above does ahead of Google Tag Manager. A serve middleware's head markup is
written after what each fetch middleware writes ahead of the script bundle,
and the module cannot tell which middleware write a vendor's tag there, so a
`[[serve]]` entry may name the prompt only for pages whose `[[fetch]]` entry
names no other middleware.

Selecting the module is refused when the settings load and by `ts config
validate` if the entries leave any page served as `text/html` without the
prompt, and the refusal names those pages. A page takes the first entry of
each phase that covers it, so each page has to take an entry of one phase or
the other that names `cmp.inmobi`. With the prompt in `[[fetch]]` alone,
every entry names it, and one with no `path` comes after any that has one. A
`[[serve]]` entry naming it for a page whose `[[fetch]]` entry names another
middleware is refused too, and so is naming it for one page in both phases,
which would write it twice.

The tag is written at the start of the head, ahead of the page's own
`<meta charset>`, and is about 3.6 KB. A browser looks for `<meta charset>`
only in the first 1024 bytes of a page, so the origin should send the
encoding in `Content-Type` as well, as `text/html; charset=utf-8`, or a page
that declares it only in the meta tag has its encoding guessed.

## What is checked

`script_url` becomes a `<script src>` on every page of the site, and a
settings document reaches the service from a config store rather than from a
person reading it. So the URL is refused, at load and by `ts config
validate`, when it is empty, is not a URL, is not `https`, is on a host other
than `cmp.inmobi.com` or `cdn.inmobi.com`, or carries a user name or
password, which every reader's browser would be sent. A host that merely
contains an allowed name, such as `cmp.inmobi.com.example`, is refused by
what it is rather than by what it looks like.

## What a consent prompt module keeps to

A consent management platform module is the one module that decides what a
reader is asked before anything else runs. This module keeps to the
following, and they are what every `[cmp]` module is held to.

1. The prompt is written before any vendor's tag, and the entry's order is
   the head's order. Which phase it is written in is the publisher's choice.
2. The IAB interfaces a page may call before the prompt arrives, `__tcfapi`
   and `__gpp`, are stubbed ahead of the loader and queue or answer every
   call, and a stub steps aside where the page already has the interface.
3. The loader is deferred and never holds up the page.
4. The reader's answer reaches the permission model through the permission
   signal modules, from where the prompt itself writes it, and through no
   other path.
5. A script address written into every page is checked at load against
   what it is, not what it looks like.
6. A selected prompt that the entries leave off any page is refused rather
   than silently left unwritten.

## Tests

The two stubs are run in a stand-in for the browser with Node's test runner,
`node --test "crates/cmp/inmobi/tests/*.test.mjs"`, which CI runs with the browser
bundle's tests. The module's own tests cover the refusals, the tag's parts
and their order, and the page a reader receives, kept as
`src/fixtures/page-change.recorded.html`.
