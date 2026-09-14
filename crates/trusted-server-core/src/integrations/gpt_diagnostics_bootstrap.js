// Request-scoped URL cleanup only. The server injects this asset exactly when
// it has already consumed and stripped at least one reserved directive.
(function () {
    "use strict";

    try {
        var href = String(location.href);
        var hashIndex = href.indexOf("#");
        var hash = hashIndex < 0 ? "" : href.slice(hashIndex);
        var beforeHash = hashIndex < 0 ? href : href.slice(0, hashIndex);
        var queryIndex = beforeHash.indexOf("?");
        if (queryIndex < 0) return;

        var pairs = beforeHash.slice(queryIndex + 1).split("&");
        var retained = [];
        var removed = false;
        for (var index = 0; index < pairs.length; index += 1) {
            var pair = pairs[index];
            var equalsIndex = pair.indexOf("=");
            var name = equalsIndex < 0 ? pair : pair.slice(0, equalsIndex);
            if (name === "ts_console") {
                removed = true;
            } else {
                retained.push(pair);
            }
        }
        if (!removed) return;

        var cleanHref = beforeHash.slice(0, queryIndex);
        var retainedQuery = retained.join("&");
        if (retainedQuery !== "") cleanHref += "?" + retainedQuery;
        history.replaceState(history.state, "", cleanHref + hash);
    } catch (_) {
        // Browser-visible cleanup cannot affect diagnostics or publisher code.
    }
})();
