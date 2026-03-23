export default function Home() {
  // Server component — process.env is available at render time
  const originHost = process.env.ORIGIN_HOST || "127.0.0.1:8888";

  return (
    <main>
      <h1>Integration Test Publisher</h1>
      <p>This is a test page for integration testing of the trusted server.</p>

      {/* Links with absolute origin URLs for attribute rewriting tests.
          The trusted server should rewrite these from origin host to proxy host. */}
      <a id="origin-link" href={`http://${originHost}/page`}>
        Origin Link
      </a>
      <img
        id="origin-img"
        src={`http://${originHost}/images/test.png`}
        alt="test"
      />

      {/* Ad slots with both data-ad-unit (preserved) and URL attributes (rewritten).
          This tests that URL rewriting works inside ad markup, not just outside it. */}
      <div id="ad-slot-1" data-ad-unit="/test/banner">
        <a href={`http://${originHost}/ad/banner-landing`}>Banner ad</a>
        <img src={`http://${originHost}/ad/banner.png`} alt="banner" />
      </div>

      <p>More page content follows the ad slot.</p>

      {/* Second ad slot */}
      <div id="ad-slot-2" data-ad-unit="/test/sidebar">
        <a href={`http://${originHost}/ad/sidebar-landing`}>Sidebar ad</a>
      </div>
    </main>
  );
}
