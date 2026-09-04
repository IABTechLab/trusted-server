# Pre-navigation Cookie Installation Design

## Problem

The audit collectors open `about:blank` so initialization scripts can be installed before publisher code runs. Cookies are explicitly scoped by domain and `/`, but `chromiumoxide::Page::set_cookie` rejects cookies without a URL while the page is still `about:blank`. Consequently, any audit using `--cookie` fails before navigation; audits without cookies are unaffected.

## Design

Build the same host-only, root-scoped `CookieParam` values, then install them through `Browser::set_cookies` before creating the page. Browser-level installation sends the explicit domain/path cookie directly to Chrome without deriving scope from the current page URL. Both verification and generation collectors use one shared helper so their behavior cannot drift.

Cookie-installation errors remain fatal and identify the affected cookie without logging its value. Page initialization, first-request authentication, browser-session reuse, and cookie scope remain unchanged.

## Testing

Add a Chrome-backed regression test that starts with `about:blank`, installs a cookie through the shared browser helper, navigates to a local HTTP fixture, and verifies the cookie is visible on the first loaded document. Run the focused CLI tests, formatting, and lint checks required for the touched crate.
