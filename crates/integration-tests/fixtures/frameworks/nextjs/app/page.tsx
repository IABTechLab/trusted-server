export default function Home() {
  return (
    <main>
      <h1>Integration Test Publisher</h1>
      <p>This is a test page for integration testing of the trusted server.</p>

      {/* Ad slot that should be rewritten by the trusted server */}
      <div id="ad-slot-1" data-ad-unit="/test/banner">
        <p>Advertisement placeholder</p>
      </div>

      <p>More page content follows the ad slot.</p>

      {/* Second ad slot */}
      <div id="ad-slot-2" data-ad-unit="/test/sidebar">
        <p>Sidebar advertisement placeholder</p>
      </div>
    </main>
  );
}
