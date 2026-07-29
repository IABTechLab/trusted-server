// Early activation bootstrap for the GPT diagnostics integration.
//
// This script intentionally owns only tab-local activation and one-time URL
// cleanup. The TypeScript integration reads the document flag below and owns
// all GPT observation, storage, API, and presentation behavior.
(function () {
    if (typeof window === "undefined") return;

    var queryName = "ts_console";
    var storageKey = "tsjs:gptDiagnostics:active";
    var activeFlag = "__tsjs_gpt_diagnostics_active";
    var active = false;
    var directiveRecognized = false;
    var url;

    try {
        url = new URL(window.location.href);
        var value = url.searchParams.get(queryName);

        if (value === "1" || value === "true") {
            active = true;
            directiveRecognized = true;
        } else if (value === "0" || value === "false") {
            active = false;
            directiveRecognized = true;
        }
    } catch (_) {
        url = undefined;
    }

    if (directiveRecognized) {
        try {
            window.sessionStorage.setItem(storageKey, active ? "1" : "0");
        } catch (_) {
            // The recognized directive still applies to this document.
        }

        if (url) {
            url.searchParams.delete(queryName);
            try {
                window.history.replaceState(
                    window.history.state,
                    "",
                    url.pathname + url.search + url.hash,
                );
            } catch (_) {
                // URL cleanup is optional and must not block diagnostics activation.
            }
        }
    } else {
        try {
            active = window.sessionStorage.getItem(storageKey) === "1";
        } catch (_) {
            active = false;
        }
    }

    window[activeFlag] = active;
})();
