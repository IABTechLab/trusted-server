export default function About() {
  const originHost = process.env.ORIGIN_HOST || "127.0.0.1:8888";

  return (
    <main>
      <h1>About Page</h1>
      <p>This is the about page for client-side navigation testing.</p>

      <a id="origin-link-about" href={`http://${originHost}/contact`}>
        Origin Link (About)
      </a>

      <div id="ad-slot-about" data-ad-unit="/test/about-banner">
        <a href={`http://${originHost}/ad/about-landing`}>About ad</a>
      </div>
    </main>
  );
}
