# 51Degrees client hints middleware

`trusted-server-middleware-51degrees` supplies the `51degrees.client_hints`
serve middleware. It asks the 51Degrees service which client hints the
reader's browser can send for each page navigation a `[[serve]]` entry
naming it covers, sending the `User-Agent` and `Sec-CH-UA` hints alone, and
leaves the hint names in the request's state for the other 51Degrees
modules. On each page a `[[serve]]` entry names it for, it writes
`Accept-CH` and `Critical-CH` on the response unless the browser already
sends every one of those hints, and `Permissions-Policy` and the
`Delegate-CH` tag that delegate those hints to the service.

`[shared.51degrees]` runs it, and `critical` in
`[middleware.51degrees.client_hints]` turns the retry of the first navigation
off. What it shares with the other 51Degrees modules is in
`crates/shared/51degrees`.

The [51Degrees guide](../../../docs/guide/integrations/51degrees.md) says
how to place and configure it.
