import type { Metadata } from "next";

// These URLs use origin.example.com to test Trusted Server URL rewriting.
// In production, Trusted Server rewrites these to the proxy host.
const ORIGIN = "https://origin.example.com:3099";

export const metadata: Metadata = {
  title: "Next.js RSC Test App",
  description: "Minimal app for testing Trusted Server RSC integration",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <head>
        <link rel="stylesheet" href={`${ORIGIN}/styles/main.css`} />
        <link rel="icon" href={`${ORIGIN}/favicon.ico`} />
      </head>
      <body>
        <nav>
          <a href={`${ORIGIN}/`}>Home</a>
          <a href={`${ORIGIN}/about`}>About</a>
          <a href={`${ORIGIN}/blog/hello-world`}>Blog</a>
        </nav>
        <main>{children}</main>
        <footer>
          <a href={`${ORIGIN}/privacy`}>Privacy Policy</a>
          <a href={`${ORIGIN}/terms`}>Terms of Service</a>
        </footer>
      </body>
    </html>
  );
}
