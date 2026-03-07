"use client";

import Script from "next/script";

export default function RouteScript({ marker }: { marker: string }) {
  return (
    <Script
      id={`route-deferred-script-${marker}`}
      strategy="afterInteractive"
      dangerouslySetInnerHTML={{
        __html: [
          `window.__routeScriptExecuted = "${marker}";`,
          `window.__routeScriptExecutionCount = (window.__routeScriptExecutionCount ?? 0) + 1;`,
        ].join("\n"),
      }}
    />
  );
}
