import Navigation from "./components/Navigation";

export const metadata = {
  title: "Test Publisher - Next.js",
  description: "Integration test page for trusted server",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <head />
      <body>
        <Navigation />
        {children}
      </body>
    </html>
  );
}
