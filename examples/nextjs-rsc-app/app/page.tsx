// Home page — produces simple RSC payloads with JSON URLs.
// The RSC Flight data will contain these URLs as string literals.
const ORIGIN = "https://origin.example.com:3099";

interface LinkItem {
  title: string;
  url: string;
  description: string;
}

// These links will appear in the RSC payload as JSON data
const links: LinkItem[] = [
  {
    title: "Documentation",
    url: `${ORIGIN}/docs`,
    description: "Learn about the platform",
  },
  {
    title: "API Reference",
    url: `${ORIGIN}/api/v1`,
    description: "Explore the REST API",
  },
  {
    title: "Dashboard",
    url: `${ORIGIN}/dashboard`,
    description: "View your analytics",
  },
];

export default function HomePage() {
  return (
    <div>
      <h1>Welcome to the Test App</h1>
      <p>
        Visit our <a href={`${ORIGIN}/getting-started`}>getting started guide</a>.
      </p>
      <ul>
        {links.map((link) => (
          <li key={link.url}>
            <a href={link.url}>{link.title}</a>
            <span> - {link.description}</span>
          </li>
        ))}
      </ul>
      <img src={`${ORIGIN}/images/hero.jpg`} alt="Hero" width={800} height={400} />
    </div>
  );
}
