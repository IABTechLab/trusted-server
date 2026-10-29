# Testlight Integration

Testlight is a disabled-by-default development integration for exercising the
proxy and HTML-rewrite seams. Enabling it requires a valid upstream endpoint.

## Auction proxy

`POST /integrations/testlight/auction` accepts a bounded JSON body. The
request must carry a valid `ts-ec` cookie. Testlight replaces `user.id`
with the derived EC ID while preserving unknown JSON fields, then sends the
request upstream as JSON. The registry removes the internal EC header before
dispatch, and Testlight explicitly disables EC-header forwarding.

The upstream response remains status- and header-preserving except for
`Content-Length`, which is removed after bounded collection. Valid JSON is
re-serialized and receives `application/json`; non-JSON bytes are returned
unchanged without claiming a JSON content type.

## Script rewrite

When `rewrite_scripts = true`, `src` and `href` values containing
`testlight.js` are replaced with `shim_src`. Other attributes and values
are unchanged. The default shim source is the registry-free unified `tsjs`
path because Testlight is an immediate bundled module.

This integration is for controlled development validation, not a production
auction provider. See [Configuration](/guide/configuration#testlight-integration)
for exact fields and [Integration Guide](/guide/integration-guide) for the
underlying extension seams.
