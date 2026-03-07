"use client";

import { useEffect } from "react";
import Link from "next/link";

export default function Navigation() {
  useEffect(() => {
    // Signal that the client router has hydrated and is ready to handle
    // SPA navigations. Browser tests wait for this before clicking links.
    document.documentElement.dataset.hydrated = "true";
  }, []);

  return (
    <nav id="site-nav">
      <Link href="/">Home</Link>
      <Link href="/about">About</Link>
      <Link href="/dashboard">Dashboard</Link>
      <Link href="/contact">Contact</Link>
    </nav>
  );
}
